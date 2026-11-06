// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! A minimal SNTP (RFC 4330) client used to measure the clock difference
//! between this computer and a domain controller.
//!
//! Kerberos refuses to work when clocks differ by more than five minutes,
//! so the wizard checks this before trying to join.

use std::net::{ToSocketAddrs, UdpSocket};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Seconds between the NTP epoch (1900) and the Unix epoch (1970).
const NTP_UNIX_OFFSET: f64 = 2_208_988_800.0;

fn ntp_timestamp(buf: &[u8], pos: usize) -> Option<f64> {
    let b = buf.get(pos..pos + 8)?;
    let secs = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    let frac = u32::from_be_bytes([b[4], b[5], b[6], b[7]]);
    Some(f64::from(secs) + f64::from(frac) / 4_294_967_296.0 - NTP_UNIX_OFFSET)
}

/// Clock offset (server minus client) in seconds, computed from the four
/// timestamps of an SNTP exchange. `t1`/`t4` are our send/receive times as
/// Unix seconds; `packet` is the 48-byte server reply.
pub fn offset_from_reply(packet: &[u8], t1: f64, t4: f64) -> Result<f64, String> {
    if packet.len() < 48 {
        return Err("short SNTP reply".into());
    }
    let mode = packet[0] & 0x07;
    if mode != 4 && mode != 5 {
        return Err(format!("unexpected SNTP mode {mode}"));
    }
    let t2 = ntp_timestamp(packet, 32).ok_or("bad receive timestamp")?;
    let t3 = ntp_timestamp(packet, 40).ok_or("bad transmit timestamp")?;
    if t3 < 1.0 {
        return Err("server clock not set".into());
    }
    Ok(f64::midpoint(t2 - t1, t3 - t4))
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// Asks `host` (UDP 123) for the time and returns its clock offset from ours
/// in seconds (positive means the server is ahead).
pub fn clock_offset(host: &str, timeout: Duration) -> Result<f64, String> {
    let addr = format!("{host}:123")
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve {host}: {e}"))?
        .next()
        .ok_or_else(|| format!("cannot resolve {host}"))?;
    let sock = UdpSocket::bind(if addr.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    })
    .map_err(|e| e.to_string())?;
    sock.set_read_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    let mut req = [0u8; 48];
    req[0] = 0x23; // LI=0, VN=4, mode=3 (client)
    let t1 = unix_now();
    sock.send_to(&req, addr).map_err(|e| e.to_string())?;
    let mut buf = [0u8; 48];
    let (n, _) = sock
        .recv_from(&mut buf)
        .map_err(|_| format!("{host} did not answer on the time port"))?;
    let t4 = unix_now();
    offset_from_reply(&buf[..n], t1, t4)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(t2: f64, t3: f64) -> Vec<u8> {
        let mut p = vec![0u8; 48];
        p[0] = 0x24; // VN=4, mode 4 (server)
        for (pos, t) in [(32usize, t2), (40usize, t3)] {
            let ntp = t + NTP_UNIX_OFFSET;
            let secs = ntp.trunc();
            let frac = (ntp - secs) * 4_294_967_296.0;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                p[pos..pos + 4].copy_from_slice(&(secs as u32).to_be_bytes());
                p[pos + 4..pos + 8].copy_from_slice(&(frac as u32).to_be_bytes());
            }
        }
        p
    }

    #[test]
    fn computes_offset() {
        // Server is 300 s ahead; symmetric 0.1 s network delay each way.
        let t1 = 1_700_000_000.0;
        let t2 = t1 + 300.0 + 0.1;
        let t3 = t2 + 0.01;
        let t4 = t1 + 0.21;
        let off = offset_from_reply(&reply(t2, t3), t1, t4).unwrap();
        assert!((off - 300.0).abs() < 0.01, "offset was {off}");
    }

    #[test]
    fn rejects_bad_packets() {
        assert!(offset_from_reply(&[0u8; 10], 0.0, 0.0).is_err());
        let mut p = reply(1.0, 1.0);
        p[0] = 0x23; // client mode
        assert!(offset_from_reply(&p, 0.0, 0.0).is_err());
        assert!(offset_from_reply(&reply(0.0, 0.0), 0.0, 0.0).is_err());
    }
}
