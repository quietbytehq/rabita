// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! A tiny DNS SRV client (RFC 1035 / RFC 2782) with no dependencies.
//!
//! Domain controllers advertise themselves as
//! `_ldap._tcp.dc._msdcs.<domain>`. We ask the system's name servers from
//! `/etc/resolv.conf` over UDP. This avoids depending on `dig`, `host` or
//! `realm` being installed before the join starts.

use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::time::Duration;

/// One SRV record.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SrvRecord {
    /// Lower is preferred.
    pub priority: u16,
    /// Higher is preferred within the same priority.
    pub weight: u16,
    /// Port.
    pub port: u16,
    /// Target host name (no trailing dot).
    pub target: String,
}

/// Why a lookup failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnsError {
    /// No name server could be found in `/etc/resolv.conf`.
    NoNameservers,
    /// Nothing answered.
    Timeout,
    /// The name does not exist (NXDOMAIN) or has no SRV records.
    NoRecords,
    /// Network or packet problem.
    Other(String),
}

impl std::fmt::Display for DnsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoNameservers => write!(f, "no DNS servers are configured on this computer"),
            Self::Timeout => write!(f, "the DNS server did not answer"),
            Self::NoRecords => write!(f, "the domain has no domain controller records in DNS"),
            Self::Other(e) => write!(f, "DNS problem: {e}"),
        }
    }
}

impl std::error::Error for DnsError {}

/// Name servers listed in `resolv.conf` text.
#[must_use]
pub fn parse_resolv_conf(text: &str) -> Vec<IpAddr> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            let rest = l.strip_prefix("nameserver")?;
            rest.split_whitespace().next()?.parse().ok()
        })
        .collect()
}

/// Builds a DNS query packet for SRV records of `name`.
#[must_use]
pub fn build_srv_query(id: u16, name: &str) -> Vec<u8> {
    let mut p = Vec::with_capacity(64);
    p.extend_from_slice(&id.to_be_bytes());
    p.extend_from_slice(&[0x01, 0x00]); // standard query, recursion desired
    p.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0]); // 1 question
    for label in name.trim_end_matches('.').split('.') {
        let bytes = label.as_bytes();
        p.push(u8::try_from(bytes.len().min(63)).unwrap_or(63));
        p.extend_from_slice(&bytes[..bytes.len().min(63)]);
    }
    p.push(0);
    p.extend_from_slice(&[0, 33]); // QTYPE SRV
    p.extend_from_slice(&[0, 1]); // QCLASS IN
    p
}

fn read_u16(buf: &[u8], pos: usize) -> Result<u16, DnsError> {
    buf.get(pos..pos + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .ok_or_else(|| DnsError::Other("truncated packet".into()))
}

/// Reads a (possibly compressed) name at `pos`. Returns the name and the
/// position just after it in the current sequence.
fn read_name(buf: &[u8], mut pos: usize) -> Result<(String, usize), DnsError> {
    let mut labels = Vec::new();
    let mut end = None;
    let mut hops = 0;
    loop {
        let len = *buf
            .get(pos)
            .ok_or_else(|| DnsError::Other("truncated name".into()))?;
        if len & 0xC0 == 0xC0 {
            let ptr = usize::from(read_u16(buf, pos)? & 0x3FFF);
            if end.is_none() {
                end = Some(pos + 2);
            }
            hops += 1;
            if hops > 32 {
                return Err(DnsError::Other("name compression loop".into()));
            }
            pos = ptr;
            continue;
        }
        pos += 1;
        if len == 0 {
            break;
        }
        let l = usize::from(len);
        let bytes = buf
            .get(pos..pos + l)
            .ok_or_else(|| DnsError::Other("truncated label".into()))?;
        labels.push(String::from_utf8_lossy(bytes).into_owned());
        pos += l;
    }
    Ok((labels.join("."), end.unwrap_or(pos)))
}

/// Parses a DNS response and returns the SRV records in the answer section,
/// sorted best first.
pub fn parse_srv_response(buf: &[u8], expected_id: u16) -> Result<Vec<SrvRecord>, DnsError> {
    if read_u16(buf, 0)? != expected_id {
        return Err(DnsError::Other("response id mismatch".into()));
    }
    let flags = read_u16(buf, 2)?;
    let rcode = flags & 0x000F;
    if rcode == 3 {
        return Err(DnsError::NoRecords);
    }
    if rcode != 0 {
        return Err(DnsError::Other(format!(
            "server returned error code {rcode}"
        )));
    }
    let qdcount = read_u16(buf, 4)?;
    let ancount = read_u16(buf, 6)?;
    let mut pos = 12;
    for _ in 0..qdcount {
        let (_, next) = read_name(buf, pos)?;
        pos = next + 4;
    }
    let mut records = Vec::new();
    for _ in 0..ancount {
        let (_, next) = read_name(buf, pos)?;
        pos = next;
        let rtype = read_u16(buf, pos)?;
        let rdlen = usize::from(read_u16(buf, pos + 8)?);
        let rdata = pos + 10;
        if rtype == 33 {
            let (target, _) = read_name(buf, rdata + 6)?;
            records.push(SrvRecord {
                priority: read_u16(buf, rdata)?,
                weight: read_u16(buf, rdata + 2)?,
                port: read_u16(buf, rdata + 4)?,
                target,
            });
        }
        pos = rdata + rdlen;
    }
    if records.is_empty() {
        return Err(DnsError::NoRecords);
    }
    records.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| b.weight.cmp(&a.weight))
            .then_with(|| a.target.cmp(&b.target))
    });
    Ok(records)
}

/// Looks up SRV records for `name` using the given name servers.
pub fn lookup_srv(
    name: &str,
    nameservers: &[IpAddr],
    timeout: Duration,
) -> Result<Vec<SrvRecord>, DnsError> {
    if nameservers.is_empty() {
        return Err(DnsError::NoNameservers);
    }
    let id = u16::try_from(std::process::id() & 0xFFFF).unwrap_or(0x1234) ^ 0x5A5A;
    let query = build_srv_query(id, name);
    let mut last = DnsError::Timeout;
    for ns in nameservers {
        let bind: SocketAddr = match ns {
            IpAddr::V4(_) => SocketAddr::from(([0, 0, 0, 0], 0)),
            IpAddr::V6(_) => SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 0)),
        };
        let sock = match UdpSocket::bind(bind) {
            Ok(s) => s,
            Err(e) => {
                last = DnsError::Other(e.to_string());
                continue;
            }
        };
        let _ = sock.set_read_timeout(Some(timeout));
        if sock.send_to(&query, SocketAddr::new(*ns, 53)).is_err() {
            last = DnsError::Other(format!("could not send to {ns}"));
            continue;
        }
        let mut buf = [0u8; 4096];
        match sock.recv_from(&mut buf) {
            Ok((n, _)) => match parse_srv_response(&buf[..n], id) {
                Ok(r) => return Ok(r),
                Err(e) => {
                    last = e;
                    if last == DnsError::NoRecords {
                        return Err(last);
                    }
                }
            },
            Err(_) => last = DnsError::Timeout,
        }
    }
    Err(last)
}

/// The SRV name domain controllers register for LDAP.
#[must_use]
pub fn dc_srv_name(domain: &str) -> String {
    format!(
        "_ldap._tcp.dc._msdcs.{}",
        domain.trim().trim_end_matches('.').to_ascii_lowercase()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_resolv_conf() {
        let ns = parse_resolv_conf(
            "# x\nnameserver 127.0.0.53\noptions edns0\nnameserver 10.0.0.1 # dc\n",
        );
        assert_eq!(
            ns,
            [
                "127.0.0.53".parse::<IpAddr>().unwrap(),
                "10.0.0.1".parse().unwrap()
            ]
        );
    }

    #[test]
    fn builds_query() {
        let q = build_srv_query(0xBEEF, "_ldap._tcp.dc._msdcs.lab.test");
        assert_eq!(&q[..2], &[0xBE, 0xEF]);
        assert_eq!(&q[2..4], &[0x01, 0x00]);
        assert_eq!(&q[4..6], &[0, 1]);
        assert_eq!(q[12], 5);
        assert_eq!(&q[13..18], b"_ldap");
        let n = q.len();
        assert_eq!(&q[n - 4..], &[0, 33, 0, 1]);
    }

    /// A hand-built response with two SRV answers using name compression.
    fn sample_response(id: u16) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&id.to_be_bytes());
        p.extend_from_slice(&[0x81, 0x80]); // response, RD, RA, no error
        p.extend_from_slice(&[0, 1, 0, 2, 0, 0, 0, 0]);
        // question: _ldap._tcp.dc._msdcs.lab.test SRV IN
        for l in ["_ldap", "_tcp", "dc", "_msdcs", "lab", "test"] {
            p.push(u8::try_from(l.len()).unwrap());
            p.extend_from_slice(l.as_bytes());
        }
        p.push(0);
        p.extend_from_slice(&[0, 33, 0, 1]);
        // answer 1: pointer to question name (offset 12)
        for (prio, weight, port, host) in [(0u16, 100u16, 389u16, "dc1"), (10, 50, 389, "dc2")] {
            p.extend_from_slice(&[0xC0, 0x0C]);
            p.extend_from_slice(&[0, 33, 0, 1]);
            p.extend_from_slice(&600u32.to_be_bytes());
            let mut rdata = Vec::new();
            rdata.extend_from_slice(&prio.to_be_bytes());
            rdata.extend_from_slice(&weight.to_be_bytes());
            rdata.extend_from_slice(&port.to_be_bytes());
            rdata.push(u8::try_from(host.len()).unwrap());
            rdata.extend_from_slice(host.as_bytes());
            // "lab.test" via pointer to offset 12 + 1+5 + 1+4 + 1+2 + 1+6 = 33
            rdata.extend_from_slice(&[0xC0, 33]);
            p.extend_from_slice(&u16::try_from(rdata.len()).unwrap().to_be_bytes());
            p.extend_from_slice(&rdata);
        }
        p
    }

    #[test]
    fn parses_srv_answers_with_compression() {
        let recs = parse_srv_response(&sample_response(7), 7).unwrap();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].target, "dc1.lab.test");
        assert_eq!(recs[0].port, 389);
        assert_eq!(recs[0].priority, 0);
        assert_eq!(recs[1].target, "dc2.lab.test");
        assert_eq!(recs[1].priority, 10);
    }

    #[test]
    fn rejects_wrong_id_and_nxdomain() {
        assert!(matches!(
            parse_srv_response(&sample_response(7), 8),
            Err(DnsError::Other(_))
        ));
        let mut nx = sample_response(1);
        nx[3] = 0x83; // rcode 3
        assert_eq!(parse_srv_response(&nx, 1), Err(DnsError::NoRecords));
    }

    #[test]
    fn lookup_fails_cleanly_without_nameservers() {
        assert_eq!(
            lookup_srv("x.test", &[], Duration::from_millis(10)),
            Err(DnsError::NoNameservers)
        );
    }

    #[test]
    fn srv_name_is_lowercased() {
        assert_eq!(
            dc_srv_name("LAB.Quietbyte.TEST."),
            "_ldap._tcp.dc._msdcs.lab.quietbyte.test"
        );
    }
}
