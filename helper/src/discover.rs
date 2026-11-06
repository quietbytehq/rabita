// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! `DiscoverDomain`: find the domain controllers for a domain name and
//! check that we can reach them.

use std::fs;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::dns;
use crate::paths::Paths;
use crate::sntp;
use crate::types::{DiscoveryResult, DomainController};

/// Normalises what the user typed into `(domain, realm)`.
#[must_use]
pub fn normalise_domain(input: &str) -> (String, String) {
    let domain = input
        .trim()
        .trim_end_matches('.')
        .trim_start_matches("\\\\")
        .to_ascii_lowercase();
    let realm = domain.to_ascii_uppercase();
    (domain, realm)
}

/// Whether `input` looks like a DNS domain name.
#[must_use]
pub fn looks_like_domain(input: &str) -> bool {
    let (d, _) = normalise_domain(input);
    !d.is_empty()
        && d.contains('.')
        && d.split('.').all(|l| {
            !l.is_empty()
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                && !l.starts_with('-')
                && !l.ends_with('-')
        })
}

fn tcp_reachable(host: &str, port: u16, timeout: Duration) -> bool {
    let Ok(mut addrs) = format!("{host}:{port}").to_socket_addrs() else {
        return false;
    };
    addrs.any(|a| TcpStream::connect_timeout(&a, timeout).is_ok())
}

/// Finds domain controllers via DNS and probes them.
///
/// `probe` controls whether network reachability and clock checks run
/// (tests turn it off).
#[must_use]
pub fn discover(input: &str, paths: &Paths, probe: bool) -> DiscoveryResult {
    let (domain, realm) = normalise_domain(input);
    let mut result = DiscoveryResult {
        domain: domain.clone(),
        realm,
        found: false,
        controllers: Vec::new(),
        problem: None,
    };
    if !looks_like_domain(&domain) {
        result.problem = Some(
            "Enter the full domain name, for example school.example.org (not the short NetBIOS name)."
                .to_owned(),
        );
        return result;
    }
    let resolv = fs::read_to_string(paths.resolv_conf()).unwrap_or_default();
    let mut nameservers = dns::parse_resolv_conf(&resolv);
    if nameservers.is_empty() {
        nameservers.push(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 53)));
    }
    let records = match dns::lookup_srv(
        &dns::dc_srv_name(&domain),
        &nameservers,
        Duration::from_secs(3),
    ) {
        Ok(r) => r,
        Err(dns::DnsError::NoRecords) => {
            result.problem = Some(format!(
                "No domain controllers were found for '{domain}'. Check the spelling, and make sure this computer uses the organization's DNS server."
            ));
            return result;
        }
        Err(e) => {
            result.problem = Some(format!("Could not look up '{domain}': {e}."));
            return result;
        }
    };
    for r in records {
        let (reachable, clock_offset_secs) = if probe {
            let reachable = tcp_reachable(&r.target, r.port, Duration::from_secs(3));
            let off = if reachable {
                sntp::clock_offset(&r.target, Duration::from_secs(3)).ok()
            } else {
                None
            };
            (reachable, off)
        } else {
            (false, None)
        };
        result.controllers.push(DomainController {
            host: r.target,
            port: r.port,
            priority: r.priority,
            weight: r.weight,
            reachable,
            clock_offset_secs,
        });
    }
    result.found = !result.controllers.is_empty();
    if probe && result.found && !result.controllers.iter().any(|c| c.reachable) {
        result.problem = Some(format!(
            "Found domain controllers for '{domain}' in DNS, but none of them could be reached. Check the network connection and firewall."
        ));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_input() {
        assert_eq!(
            normalise_domain("  LAB.QuietByte.test. "),
            (
                "lab.quietbyte.test".to_owned(),
                "LAB.QUIETBYTE.TEST".to_owned()
            )
        );
    }

    #[test]
    fn validates_domain_shape() {
        assert!(looks_like_domain("lab.quietbyte.test"));
        assert!(looks_like_domain("school.example.org"));
        assert!(!looks_like_domain("LAB"));
        assert!(!looks_like_domain("bad_name.test"));
        assert!(!looks_like_domain("-x.test"));
        assert!(!looks_like_domain(""));
    }

    #[test]
    fn rejects_short_names_with_advice() {
        let dir = tempfile::tempdir().unwrap();
        let res = discover("LAB", &Paths::under(dir.path()), false);
        assert!(!res.found);
        assert!(res.problem.unwrap().contains("full domain name"));
    }

    #[test]
    fn reports_dns_problems_in_plain_language() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        // Point at a name server that will not answer (unroutable address).
        let rc = paths.resolv_conf();
        fs::create_dir_all(rc.parent().unwrap()).unwrap();
        fs::write(&rc, "nameserver 192.0.2.1\n").unwrap();
        let res = discover("nowhere.invalid", &paths, false);
        assert!(!res.found);
        assert!(res
            .problem
            .unwrap()
            .starts_with("Could not look up 'nowhere.invalid'"));
    }
}
