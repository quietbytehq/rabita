// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! Pointing this computer at the organization's DNS server, through
//! `NetworkManager`, and putting the original setting back.
//!
//! Domain computers must use the organization's DNS server, or they cannot
//! find the domain at all. When "Your organization" cannot find the domain,
//! the wizard can look for a DNS server on the local network that knows it
//! ([`find_dns_servers`]) and set it on the active connection
//! ([`set_dns_server`]). The original values are saved in the state file
//! first; Undo and Leave call [`restore_dns`].
//!
//! Everything goes through `nmcli`, so the change shows up in the desktop's
//! own network settings and survives reboots like any other setting.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use crate::backup::{State, Store};
use crate::cmd::{CommandSpec, Runner};
use crate::discover;
use crate::dns;
use crate::join::JobContext;
use crate::types::{DnsChange, DnsResult, DnsSearch};

const NOT_NETWORKMANAGER: &str = "This computer's network is not managed by NetworkManager, so Rabita can't change its DNS server. Set the DNS server in the network settings, then choose Find again.";

/// The network connection that carries the default route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveConnection {
    /// `NetworkManager` connection id.
    pub uuid: String,
    /// Name people see, e.g. "Wired connection 1".
    pub name: String,
    /// Device, e.g. `enp1s0`.
    pub device: String,
    /// This computer's IPv4 address and prefix length, if any.
    pub address: Option<(Ipv4Addr, u8)>,
    /// The default gateway, if any.
    pub gateway: Option<Ipv4Addr>,
}

/// DNS settings of a connection, as `nmcli -g` prints them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DnsSettings {
    /// `ipv4.dns`
    pub dns: String,
    /// `ipv4.dns-search`
    pub dns_search: String,
    /// `ipv4.ignore-auto-dns`
    pub ignore_auto_dns: String,
    /// `ipv6.ignore-auto-dns`
    pub ipv6_ignore_auto_dns: String,
}

/// Undoes `nmcli -g` escaping (`\\:` for `:`, `\\\\` for `\\`).
fn unescape(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The device of the default route, from `ip route show default`.
#[must_use]
pub fn parse_default_route_device(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        if words.next()? != "default" {
            return None;
        }
        let mut words = line.split_whitespace();
        words.find(|w| *w == "dev")?;
        words.next().map(str::to_owned)
    })
}

/// Parses `nmcli -g GENERAL.CON-UUID,GENERAL.CONNECTION,IP4.ADDRESS,IP4.GATEWAY
/// device show <dev>` output.
#[must_use]
pub fn parse_device_show(device: &str, output: &str) -> Option<ActiveConnection> {
    let lines: Vec<&str> = output.lines().collect();
    let uuid = lines.first().map(|s| unescape(s.trim()))?;
    if uuid.is_empty() {
        return None;
    }
    let address = lines.get(2).and_then(|l| {
        let first = l.split('|').next()?.trim();
        let (ip, prefix) = first.split_once('/')?;
        Some((ip.parse().ok()?, prefix.parse().ok()?))
    });
    let gateway = lines.get(3).and_then(|l| l.trim().parse().ok());
    Some(ActiveConnection {
        uuid,
        name: lines.get(1).map(|s| unescape(s.trim())).unwrap_or_default(),
        device: device.to_owned(),
        address,
        gateway,
    })
}

/// Finds the connection `NetworkManager` uses for the default route.
pub fn active_connection(runner: &dyn Runner) -> Result<ActiveConnection, String> {
    if !runner.which("nmcli") {
        return Err(NOT_NETWORKMANAGER.to_owned());
    }
    let routes = runner
        .run(&CommandSpec::new("ip", &["route", "show", "default"]))
        .map_err(|e| e.to_string())?;
    let device = parse_default_route_device(&routes.stdout)
        .ok_or("This computer is not connected to a network. Connect it first, then try again.")?;
    let out = runner
        .run(&CommandSpec::new(
            "nmcli",
            &[
                "-g",
                "GENERAL.CON-UUID,GENERAL.CONNECTION,IP4.ADDRESS,IP4.GATEWAY",
                "device",
                "show",
                &device,
            ],
        ))
        .map_err(|e| e.to_string())?;
    if !out.success() {
        return Err(NOT_NETWORKMANAGER.to_owned());
    }
    parse_device_show(&device, &out.stdout).ok_or_else(|| NOT_NETWORKMANAGER.to_owned())
}

/// Reads a connection's current DNS settings.
pub fn read_dns(runner: &dyn Runner, uuid: &str) -> Result<DnsSettings, String> {
    let out = runner
        .run(&CommandSpec::new(
            "nmcli",
            &[
                "-g",
                "ipv4.dns,ipv4.dns-search,ipv4.ignore-auto-dns,ipv6.ignore-auto-dns",
                "connection",
                "show",
                uuid,
            ],
        ))
        .map_err(|e| e.to_string())?;
    if !out.success() {
        return Err(format!(
            "Could not read the network settings: {}",
            out.combined()
        ));
    }
    let lines: Vec<String> = out.stdout.lines().map(|l| unescape(l.trim())).collect();
    let get = |i: usize| lines.get(i).cloned().unwrap_or_default();
    Ok(DnsSettings {
        dns: get(0),
        dns_search: get(1),
        ignore_auto_dns: if get(2).is_empty() {
            "no".into()
        } else {
            get(2)
        },
        ipv6_ignore_auto_dns: if get(3).is_empty() {
            "no".into()
        } else {
            get(3)
        },
    })
}

/// Writes DNS settings to a connection and applies them without
/// disconnecting.
pub fn apply_dns(
    runner: &dyn Runner,
    uuid: &str,
    device: &str,
    settings: &DnsSettings,
) -> Result<(), String> {
    let modify = runner
        .run(&CommandSpec::new(
            "nmcli",
            &[
                "connection",
                "modify",
                uuid,
                "ipv4.dns",
                &settings.dns,
                "ipv4.dns-search",
                &settings.dns_search,
                "ipv4.ignore-auto-dns",
                &settings.ignore_auto_dns,
                "ipv6.ignore-auto-dns",
                &settings.ipv6_ignore_auto_dns,
            ],
        ))
        .map_err(|e| e.to_string())?;
    if !modify.success() {
        return Err(format!(
            "NetworkManager did not accept the change: {}",
            modify.combined()
        ));
    }
    let reapply = runner
        .run(&CommandSpec::new("nmcli", &["device", "reapply", device]))
        .map_err(|e| e.to_string())?;
    if reapply.success() {
        return Ok(());
    }
    // Some drivers can't reapply in place; reconnecting works everywhere.
    let up = runner
        .run(
            &CommandSpec::new("nmcli", &["connection", "up", uuid])
                .timeout(Duration::from_secs(60)),
        )
        .map_err(|e| e.to_string())?;
    if up.success() {
        Ok(())
    } else {
        Err(format!(
            "NetworkManager could not apply the change: {}",
            up.combined()
        ))
    }
}

/// Addresses to ask when looking for the DNS server: the gateway first,
/// then every other host on this computer's /24 (or smaller) network.
#[must_use]
pub fn candidate_addresses(conn: &ActiveConnection) -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    if let Some(gw) = conn.gateway {
        out.push(gw);
    }
    if let Some((ip, prefix)) = conn.address {
        let prefix = prefix.clamp(24, 30);
        let mask = u32::MAX << (32 - u32::from(prefix));
        let network = u32::from(ip) & mask;
        let broadcast = network | !mask;
        for host in network + 1..broadcast {
            let addr = Ipv4Addr::from(host);
            if addr != ip && Some(addr) != conn.gateway {
                out.push(addr);
            }
        }
    }
    out
}

/// `FindDnsServers`: looks on the local network for DNS servers that know
/// `domain`. Changes nothing.
#[must_use]
pub fn find_dns_servers(ctx: &JobContext<'_>, domain: &str) -> DnsSearch {
    let (domain, _) = discover::normalise_domain(domain);
    let mut result = DnsSearch::default();
    if !discover::looks_like_domain(&domain) {
        result.problem =
            Some("Enter the full domain name first, for example school.example.org.".to_owned());
        return result;
    }
    let conn = match active_connection(ctx.runner) {
        Ok(c) => c,
        Err(e) => {
            result.problem = Some(e);
            return result;
        }
    };
    result.connection_name = Some(conn.name.clone());
    result.device = Some(conn.device.clone());
    let Some(net) = ctx.network else {
        result.problem = Some("Network checks are turned off.".to_owned());
        return result;
    };
    let addresses = candidate_addresses(&conn);
    result.scanned = addresses.len();
    result.candidates = net
        .probe_dns(&addresses, &dns::dc_srv_name(&domain))
        .iter()
        .map(ToString::to_string)
        .collect();
    result
}

fn fail(message: impl Into<String>) -> DnsResult {
    DnsResult {
        ok: false,
        message: message.into(),
    }
}

/// `SetDnsServer`: makes `server` this computer's DNS server on the active
/// connection, after checking it knows `domain`. The original settings are
/// saved first so Leave or Undo can put them back.
#[must_use]
pub fn set_dns_server(ctx: &JobContext<'_>, domain: &str, server: &str) -> DnsResult {
    let (domain, _) = discover::normalise_domain(domain);
    if !discover::looks_like_domain(&domain) {
        return fail("Enter the full domain name first, for example school.example.org.");
    }
    let Ok(ip) = server.trim().parse::<IpAddr>() else {
        return fail(format!(
            "“{}” is not an IP address. It should look like 10.0.0.10.",
            server.trim()
        ));
    };
    let srv_name = dns::dc_srv_name(&domain);
    // Check before changing anything, so a typo can't break name lookups.
    if let Some(net) = ctx.network {
        if !net.srv_via(ip, &srv_name) {
            return fail(format!(
                "{ip} did not answer for {domain}. Check the address with your IT team, or choose Detect."
            ));
        }
    }
    let conn = match active_connection(ctx.runner) {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    let store = Store::new(ctx.paths.clone());
    let mut state = store.load().unwrap_or_default();
    // Changing a different connection than last time: put that one back first.
    if state
        .dns_change
        .as_ref()
        .is_some_and(|c| c.connection_uuid != conn.uuid)
    {
        let undone = restore_dns_inner(ctx.runner, &store, &mut state);
        if !undone.ok {
            return undone;
        }
    }
    let mut change = match state.dns_change.take() {
        Some(existing) => existing,
        None => match read_dns(ctx.runner, &conn.uuid) {
            Ok(original) => DnsChange {
                connection_uuid: conn.uuid.clone(),
                connection_name: conn.name.clone(),
                device: conn.device.clone(),
                original_dns: original.dns,
                original_dns_search: original.dns_search,
                original_ignore_auto_dns: original.ignore_auto_dns,
                original_ipv6_ignore_auto_dns: original.ipv6_ignore_auto_dns,
                ..DnsChange::default()
            },
            Err(e) => return fail(e),
        },
    };
    change.server = ip.to_string();
    change.domain.clone_from(&domain);
    // Record the originals before touching anything.
    state.dns_change = Some(change.clone());
    if let Err(e) = store.save(&state) {
        return fail(format!("Could not save Rabita's state: {e}"));
    }
    let wanted = DnsSettings {
        dns: change.server.clone(),
        dns_search: domain.clone(),
        ignore_auto_dns: "yes".into(),
        ipv6_ignore_auto_dns: "yes".into(),
    };
    if let Err(e) = apply_dns(ctx.runner, &conn.uuid, &conn.device, &wanted) {
        return fail(e);
    }
    let seen = ctx.network.is_none_or(|net| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if net.resolves(&srv_name, ctx.paths) {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    });
    DnsResult {
        ok: true,
        message: if seen {
            format!(
                "This computer now uses {ip} as its DNS server on “{}”. Leaving the domain puts the old setting back.",
                conn.name
            )
        } else {
            format!(
                "This computer now uses {ip} as its DNS server on “{}”, but can't see {domain} yet. Wait a moment, then choose Find again.",
                conn.name
            )
        },
    }
}

pub(crate) fn restore_dns_inner(
    runner: &dyn Runner,
    store: &Store,
    state: &mut State,
) -> DnsResult {
    let Some(change) = state.dns_change.clone() else {
        return DnsResult {
            ok: true,
            message: "Rabita has not changed the DNS server.".to_owned(),
        };
    };
    let original = DnsSettings {
        dns: change.original_dns.clone(),
        dns_search: change.original_dns_search.clone(),
        ignore_auto_dns: change.original_ignore_auto_dns.clone(),
        ipv6_ignore_auto_dns: change.original_ipv6_ignore_auto_dns.clone(),
    };
    if let Err(e) = apply_dns(runner, &change.connection_uuid, &change.device, &original) {
        // A connection that no longer exists has nothing to restore.
        if !e.to_ascii_lowercase().contains("unknown connection") {
            return fail(format!("Could not put the original DNS setting back: {e}"));
        }
    }
    state.dns_change = None;
    if let Err(e) = store.save(state) {
        return fail(format!("Could not save Rabita's state: {e}"));
    }
    DnsResult {
        ok: true,
        message: format!(
            "Put the original DNS setting back on “{}”.",
            change.connection_name
        ),
    }
}

/// `RestoreDns`: puts back the DNS settings Rabita changed, if any.
#[must_use]
pub fn restore_dns(ctx: &JobContext<'_>) -> DnsResult {
    let store = Store::new(ctx.paths.clone());
    let mut state = store.load().unwrap_or_default();
    restore_dns_inner(ctx.runner, &store, &mut state)
}

/// Resolves `name` with this computer's own resolver (what `realm` and
/// SSSD will use).
#[must_use]
pub fn system_resolves(name: &str, paths: &crate::paths::Paths) -> bool {
    let resolv = std::fs::read_to_string(paths.resolv_conf()).unwrap_or_default();
    let mut servers = dns::parse_resolv_conf(&resolv);
    if servers.is_empty() {
        servers.push(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 53)));
    }
    dns::lookup_srv(name, &servers, Duration::from_secs(2)).is_ok()
}

/// Asks the given IPv4 hosts on port 53 at once (see [`dns::probe_servers`]).
#[must_use]
pub fn probe_hosts(hosts: &[Ipv4Addr], name: &str) -> Vec<Ipv4Addr> {
    let addrs: Vec<SocketAddr> = hosts.iter().map(|h| SocketAddr::from((*h, 53))).collect();
    dns::probe_servers(&addrs, name, Duration::from_millis(1500))
        .into_iter()
        .filter_map(|a| match a.ip() {
            IpAddr::V4(v4) => Some(v4),
            IpAddr::V6(_) => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::FakeRunner;
    use crate::discover::Network;
    use crate::paths::Paths;
    use crate::progress::Collector;
    use crate::types::DiscoveryResult;

    const ROUTE: &str =
        "default via 192.168.1.1 dev enp1s0 proto dhcp src 192.168.1.50 metric 100\n";
    const DEVICE: &str = "6f2a-11\nWired connection 1\n192.168.1.50/24\n192.168.1.1\n";
    const ORIGINAL: &str = "\n\nno\nno\n";

    struct FakeNet {
        knows: bool,
        found: Vec<Ipv4Addr>,
    }

    impl Network for FakeNet {
        fn discover(&self, _: &str, _: &dyn Runner, _: &Paths) -> DiscoveryResult {
            unreachable!("not used by DNS changes")
        }
        fn clock_offset(&self, _: &str) -> Option<f64> {
            None
        }
        fn srv_via(&self, _: IpAddr, _: &str) -> bool {
            self.knows
        }
        fn probe_dns(&self, hosts: &[Ipv4Addr], _: &str) -> Vec<Ipv4Addr> {
            hosts
                .iter()
                .copied()
                .filter(|h| self.found.contains(h))
                .collect()
        }
        fn resolves(&self, _: &str, _: &Paths) -> bool {
            true
        }
    }

    fn nm_runner() -> FakeRunner {
        let fake = FakeRunner::new();
        fake.ok("ip route", ROUTE)
            .ok("nmcli -g", DEVICE)
            .ok("nmcli -g", ORIGINAL);
        fake
    }

    #[test]
    fn parses_routes_and_devices() {
        assert_eq!(parse_default_route_device(ROUTE).as_deref(), Some("enp1s0"));
        assert_eq!(parse_default_route_device("10.0.0.0/8 dev x\n"), None);
        let conn = parse_device_show(
            "wlp2s0",
            "abc\nSchool\\: Staff Wi-Fi\n10.20.30.40/22 | 10.1.1.1/8\n10.20.28.1\n",
        )
        .unwrap();
        assert_eq!(conn.name, "School: Staff Wi-Fi");
        assert_eq!(conn.address, Some((Ipv4Addr::new(10, 20, 30, 40), 22)));
        assert_eq!(conn.gateway, Some(Ipv4Addr::new(10, 20, 28, 1)));
        assert_eq!(parse_device_show("x", "\n\n"), None, "unmanaged device");
    }

    #[test]
    fn candidates_are_the_gateway_then_the_local_network() {
        let conn = parse_device_show("enp1s0", DEVICE).unwrap();
        let c = candidate_addresses(&conn);
        assert_eq!(c[0], Ipv4Addr::new(192, 168, 1, 1));
        assert_eq!(c.len(), 253, "254 hosts minus this computer");
        assert!(!c[1..].contains(&Ipv4Addr::new(192, 168, 1, 50)));
        assert!(!c[1..].contains(&Ipv4Addr::new(192, 168, 1, 1)));
        // A big network is narrowed to this computer's /24.
        let big = ActiveConnection {
            address: Some((Ipv4Addr::new(10, 20, 30, 40), 16)),
            gateway: None,
            ..conn
        };
        let c = candidate_addresses(&big);
        assert_eq!(c.first(), Some(&Ipv4Addr::new(10, 20, 30, 1)));
        assert_eq!(c.last(), Some(&Ipv4Addr::new(10, 20, 30, 254)));
    }

    fn ctx<'a>(
        runner: &'a FakeRunner,
        paths: &'a Paths,
        sink: &'a Collector,
        net: &'a FakeNet,
    ) -> JobContext<'a> {
        JobContext {
            runner,
            paths,
            sink,
            network: Some(net),
        }
    }

    #[test]
    fn refuses_a_server_that_does_not_know_the_domain_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let fake = nm_runner();
        let sink = Collector::new();
        let net = FakeNet {
            knows: false,
            found: vec![],
        };
        let r = set_dns_server(&ctx(&fake, &paths, &sink, &net), "lab.test", "10.0.0.99");
        assert!(!r.ok);
        assert_eq!(
            r.message,
            "10.0.0.99 did not answer for lab.test. Check the address with your IT team, or choose Detect."
        );
        assert!(fake.calls().is_empty(), "{:?}", fake.call_lines());
        let bad = set_dns_server(&ctx(&fake, &paths, &sink, &net), "lab.test", "ten.0.0.1");
        assert_eq!(
            bad.message,
            "“ten.0.0.1” is not an IP address. It should look like 10.0.0.10."
        );
    }

    #[test]
    fn sets_the_server_and_records_the_original_for_leave() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let fake = nm_runner();
        let sink = Collector::new();
        let net = FakeNet {
            knows: true,
            found: vec![],
        };
        let r = set_dns_server(&ctx(&fake, &paths, &sink, &net), "LAB.test", "10.0.0.10");
        assert!(r.ok, "{}", r.message);
        assert_eq!(
            r.message,
            "This computer now uses 10.0.0.10 as its DNS server on “Wired connection 1”. Leaving the domain puts the old setting back."
        );
        let lines = fake.call_lines();
        assert!(lines.contains(
            &"nmcli connection modify 6f2a-11 ipv4.dns 10.0.0.10 ipv4.dns-search lab.test ipv4.ignore-auto-dns yes ipv6.ignore-auto-dns yes".to_owned()
        ), "{lines:?}");
        assert!(lines.contains(&"nmcli device reapply enp1s0".to_owned()));
        let state = Store::new(paths.clone()).load().unwrap();
        let change = state.dns_change.clone().unwrap();
        assert_eq!(change.server, "10.0.0.10");
        assert_eq!(change.connection_name, "Wired connection 1");
        assert_eq!(change.original_dns, "");
        assert_eq!(change.original_ignore_auto_dns, "no");
        assert!(
            state.restore_pending(),
            "not joined yet: Leave/Undo must put it back"
        );

        // A second change keeps the first originals (no re-read of settings).
        let second = FakeRunner::new();
        second.ok("ip route", ROUTE).ok("nmcli -g", DEVICE);
        let r = set_dns_server(&ctx(&second, &paths, &sink, &net), "lab.test", "10.0.0.11");
        assert!(r.ok);
        assert_eq!(
            second
                .call_lines()
                .iter()
                .filter(|l| l.starts_with("nmcli -g"))
                .count(),
            1,
            "only the device is read, not the (already changed) DNS settings"
        );
        let change = Store::new(paths.clone())
            .load()
            .unwrap()
            .dns_change
            .unwrap();
        assert_eq!(
            (
                change.server.as_str(),
                change.original_ignore_auto_dns.as_str()
            ),
            ("10.0.0.11", "no")
        );

        // Undo puts back exactly what was there.
        let undo = FakeRunner::new();
        let r = restore_dns(&ctx(&undo, &paths, &sink, &net));
        assert!(r.ok, "{}", r.message);
        assert_eq!(
            r.message,
            "Put the original DNS setting back on “Wired connection 1”."
        );
        assert_eq!(
            undo.call_lines()[0],
            "nmcli connection modify 6f2a-11 ipv4.dns  ipv4.dns-search  ipv4.ignore-auto-dns no ipv6.ignore-auto-dns no"
        );
        assert_eq!(Store::new(paths.clone()).load().unwrap().dns_change, None);
        assert!(restore_dns(&ctx(&FakeRunner::new(), &paths, &sink, &net))
            .message
            .contains("has not changed"));
    }

    #[test]
    fn reconnects_when_reapply_is_not_supported() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let fake = nm_runner();
        fake.fail("nmcli device", "Error: Reapply failed");
        let sink = Collector::new();
        let net = FakeNet {
            knows: true,
            found: vec![],
        };
        let r = set_dns_server(&ctx(&fake, &paths, &sink, &net), "lab.test", "10.0.0.10");
        assert!(r.ok, "{}", r.message);
        assert!(fake
            .call_lines()
            .contains(&"nmcli connection up 6f2a-11".to_owned()));
    }

    #[test]
    fn explains_when_networkmanager_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let fake = FakeRunner::new();
        fake.installed(&["ip"]);
        let sink = Collector::new();
        let net = FakeNet {
            knows: true,
            found: vec![],
        };
        let r = set_dns_server(&ctx(&fake, &paths, &sink, &net), "lab.test", "10.0.0.10");
        assert!(!r.ok);
        assert!(r.message.contains("not managed by NetworkManager"));
        let s = find_dns_servers(&ctx(&fake, &paths, &sink, &net), "lab.test");
        assert!(s.problem.unwrap().contains("not managed by NetworkManager"));
    }

    #[test]
    fn finds_dns_servers_that_know_the_domain() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let fake = FakeRunner::new();
        fake.ok("ip route", ROUTE).ok("nmcli -g", DEVICE);
        let sink = Collector::new();
        let net = FakeNet {
            knows: true,
            found: vec![
                Ipv4Addr::new(192, 168, 1, 10),
                Ipv4Addr::new(192, 168, 1, 11),
            ],
        };
        let s = find_dns_servers(&ctx(&fake, &paths, &sink, &net), "lab.test");
        assert_eq!(s.problem, None);
        assert_eq!(s.candidates, ["192.168.1.10", "192.168.1.11"]);
        assert_eq!(s.scanned, 253);
        assert_eq!(s.connection_name.as_deref(), Some("Wired connection 1"));
        assert!(find_dns_servers(&ctx(&fake, &paths, &sink, &net), "LAB")
            .problem
            .is_some());
    }
}
