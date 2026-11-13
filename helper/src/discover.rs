// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! `DiscoverDomain`: find the domain controllers for a domain name, probe
//! them, and pick the healthiest one.
//!
//! Real domains have many DCs, some of which no longer exist (stale DNS
//! records) and some of which have wrong clocks. The ranking here is:
//!
//! 1. DCs in the client's own AD site (from
//!    `_ldap._tcp.<site>._sites.dc._msdcs.<domain>`, site name via `adcli`)
//! 2. reachable DCs whose clock agrees with the majority of DCs
//! 3. smallest clock offset, then SRV priority/weight
//!
//! Unreachable DCs are flagged as possibly stale records; DCs more than
//! [`CLOCK_WARN_SECS`] off are flagged as warnings; DCs whose clock
//! disagrees with the majority are never used for time sync.

use std::fs;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::cmd::{CommandSpec, Runner};
use crate::dns;
use crate::paths::Paths;
use crate::sntp;
use crate::types::{DiscoveryResult, DomainController};

/// A DC whose clock is further than this from ours gets a warning.
pub const CLOCK_WARN_SECS: f64 = 60.0;
/// DCs whose offsets are within this of each other are "in agreement".
pub const CLOCK_AGREE_SECS: f64 = 60.0;

/// The network probes a join needs. Jobs take one of these so tests can
/// replay a real domain's layout (stale records, bad clocks) offline.
pub trait Network: Send + Sync {
    /// Finds, probes and ranks the domain controllers of `domain`.
    fn discover(&self, domain: &str, runner: &dyn Runner, paths: &Paths) -> DiscoveryResult;
    /// Measures one domain controller's clock offset in seconds.
    fn clock_offset(&self, host: &str) -> Option<f64>;
}

/// The real network: DNS, TCP and SNTP.
#[derive(Debug, Default, Clone, Copy)]
pub struct LiveNetwork;

impl Network for LiveNetwork {
    fn discover(&self, domain: &str, runner: &dyn Runner, paths: &Paths) -> DiscoveryResult {
        discover(domain, runner, paths, true)
    }

    fn clock_offset(&self, host: &str) -> Option<f64> {
        sntp::clock_offset(host, Duration::from_secs(3)).ok()
    }
}

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

/// The SRV name for DCs serving one AD site.
#[must_use]
pub fn site_srv_name(domain: &str, site: &str) -> String {
    format!(
        "_ldap._tcp.{}._sites.dc._msdcs.{}",
        site.trim(),
        domain.trim().trim_end_matches('.').to_ascii_lowercase()
    )
}

/// Reads `computer-site = X` from `adcli info` output.
#[must_use]
pub fn parse_adcli_site(output: &str) -> Option<String> {
    output.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        if k.trim() == "computer-site" {
            let v = v.trim();
            (!v.is_empty()).then(|| v.to_owned())
        } else {
            None
        }
    })
}

/// Asks `adcli` which AD site this computer is in (needs `adcli`
/// installed; on a fresh machine it is not, and the site stays unknown
/// until the join installs it).
#[must_use]
pub fn client_site(runner: &dyn Runner, domain: &str) -> Option<String> {
    if !runner.which("adcli") {
        return None;
    }
    let out = runner
        .run(
            &CommandSpec::new("adcli", &["info", "--domain", domain])
                .timeout(Duration::from_secs(20)),
        )
        .ok()?;
    if !out.success() {
        return None;
    }
    parse_adcli_site(&out.stdout)
}

fn tcp_reachable(host: &str, port: u16, timeout: Duration) -> bool {
    let Ok(mut addrs) = format!("{host}:{port}").to_socket_addrs() else {
        return false;
    };
    addrs.any(|a| TcpStream::connect_timeout(&a, timeout).is_ok())
}

/// Probes every DC in parallel: TCP to the LDAP port, then SNTP.
fn probe_all(controllers: &mut [DomainController]) {
    std::thread::scope(|scope| {
        let handles: Vec<_> = controllers
            .iter()
            .map(|dc| {
                let host = dc.host.clone();
                let port = dc.port;
                scope.spawn(move || {
                    let reachable = tcp_reachable(&host, port, Duration::from_secs(3));
                    let offset = if reachable {
                        sntp::clock_offset(&host, Duration::from_secs(3)).ok()
                    } else {
                        None
                    };
                    (reachable, offset)
                })
            })
            .collect();
        for (dc, h) in controllers.iter_mut().zip(handles) {
            if let Ok((reachable, offset)) = h.join() {
                dc.reachable = reachable;
                dc.clock_offset_secs = offset;
            }
        }
    });
}

fn fmt_offset(secs: f64) -> String {
    if secs >= 0.0 {
        format!("+{secs:.0} s")
    } else {
        format!("{secs:.0} s")
    }
}

/// Finds the clock offset the largest group of DCs agrees on: the median
/// of the biggest cluster of offsets within [`CLOCK_AGREE_SECS`] of each
/// other (ties: the cluster closest to our own clock).
#[must_use]
pub fn majority_offset(offsets: &[f64]) -> Option<f64> {
    if offsets.is_empty() {
        return None;
    }
    let mut best: Option<(usize, f64, Vec<f64>)> = None; // (size, |centre|, members)
    for &centre in offsets {
        let members: Vec<f64> = offsets
            .iter()
            .copied()
            .filter(|o| (o - centre).abs() <= CLOCK_AGREE_SECS)
            .collect();
        let size = members.len();
        let better = match &best {
            None => true,
            Some((bs, bc, _)) => size > *bs || (size == *bs && centre.abs() < *bc),
        };
        if better {
            best = Some((size, centre.abs(), members));
        }
    }
    let (_, _, mut members) = best?;
    members.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = members.len();
    Some(if n % 2 == 1 {
        members[n / 2]
    } else {
        f64::midpoint(members[n / 2 - 1], members[n / 2])
    })
}

/// What [`rank_controllers`] worked out.
#[derive(Debug, Clone, PartialEq)]
pub struct Ranking {
    /// Host of the healthiest DC, if any is reachable.
    pub recommended: Option<String>,
    /// Offset the majority of DCs agree on.
    pub majority_offset: Option<f64>,
    /// Plain-language summary warnings.
    pub warnings: Vec<String>,
}

/// Sorts `controllers` best first, fills in `in_client_site`,
/// `recommended` and per-DC `warning`, and returns the summary.
///
/// `site_hosts` are the hosts that serve the client's site (empty if
/// unknown). Pure: no network, so the exact lab scenario can be tested.
#[must_use]
pub fn rank_controllers(controllers: &mut [DomainController], site_hosts: &[String]) -> Ranking {
    for dc in controllers.iter_mut() {
        dc.in_client_site = site_hosts.iter().any(|h| h.eq_ignore_ascii_case(&dc.host));
        dc.recommended = false;
        dc.warning = None;
    }
    let offsets: Vec<f64> = controllers
        .iter()
        .filter(|d| d.reachable)
        .filter_map(|d| d.clock_offset_secs)
        .collect();
    let majority = majority_offset(&offsets);
    let agrees = |dc: &DomainController| -> bool {
        match (dc.clock_offset_secs, majority) {
            (Some(o), Some(m)) => (o - m).abs() <= CLOCK_AGREE_SECS,
            _ => true, // unmeasured: no evidence against it
        }
    };

    let mut warnings = Vec::new();
    let mut unreachable = Vec::new();
    let mut clock_warnings = Vec::new();
    for dc in controllers.iter_mut() {
        if !dc.reachable {
            dc.warning = Some(
                "Could not be reached. This may be a stale DNS record for a domain controller that no longer exists."
                    .to_owned(),
            );
            unreachable.push(dc.host.clone());
            continue;
        }
        if let Some(o) = dc.clock_offset_secs {
            let disagrees = !agrees(dc);
            if o.abs() > CLOCK_WARN_SECS || disagrees {
                let mut w = format!("Its clock is {} from this computer's", fmt_offset(o));
                if disagrees {
                    w.push_str(" and disagrees with the other domain controllers. It will not be used to set the time");
                }
                w.push('.');
                dc.warning = Some(w);
                clock_warnings.push(format!("{} ({})", dc.host, fmt_offset(o)));
            }
        }
    }

    // Rank: reachable, in site, agrees with majority, measured, |offset|, priority, weight, host.
    controllers.sort_by(|a, b| {
        let key = |d: &DomainController| {
            (
                !d.reachable,
                !d.in_client_site,
                !agrees(d),
                d.clock_offset_secs.is_none(),
                d.clock_offset_secs.map_or(f64::MAX, f64::abs),
                d.priority,
                u16::MAX - d.weight,
            )
        };
        let (ka, kb) = (key(a), key(b));
        ka.0.cmp(&kb.0)
            .then(ka.1.cmp(&kb.1))
            .then(ka.2.cmp(&kb.2))
            .then(ka.3.cmp(&kb.3))
            .then(ka.4.partial_cmp(&kb.4).unwrap_or(std::cmp::Ordering::Equal))
            .then(ka.5.cmp(&kb.5))
            .then(ka.6.cmp(&kb.6))
            .then(a.host.cmp(&b.host))
    });

    let recommended = controllers
        .first()
        .filter(|d| d.reachable)
        .map(|d| d.host.clone());
    if let Some(dc) = controllers.first_mut() {
        if dc.reachable {
            dc.recommended = true;
        }
    }

    if !unreachable.is_empty() {
        warnings.push(format!(
            "{} of {} domain controllers could not be reached (possibly stale DNS records): {}.",
            unreachable.len(),
            controllers.len(),
            unreachable.join(", ")
        ));
    }
    if !clock_warnings.is_empty() {
        let mut w = format!(
            "{} domain controller(s) have clocks more than {} s off: {}.",
            clock_warnings.len(),
            CLOCK_WARN_SECS,
            clock_warnings.join(", ")
        );
        if let Some(r) = &recommended {
            w.push_str(" This computer will use ");
            w.push_str(r);
            w.push_str(" for time.");
        }
        warnings.push(w);
    }
    Ranking {
        recommended,
        majority_offset: majority,
        warnings,
    }
}

/// Finds domain controllers via DNS, probes them and ranks them.
///
/// `probe` controls whether network reachability and clock checks run
/// (tests turn it off). `runner` is used to ask `adcli` for the client site.
#[must_use]
#[allow(clippy::too_many_lines)] // lookup, site, probe, rank: one straight line
pub fn discover(input: &str, runner: &dyn Runner, paths: &Paths, probe: bool) -> DiscoveryResult {
    let (domain, realm) = normalise_domain(input);
    let mut result = DiscoveryResult {
        domain: domain.clone(),
        realm,
        found: false,
        controllers: Vec::new(),
        problem: None,
        client_site: None,
        recommended: None,
        majority_clock_offset_secs: None,
        warnings: Vec::new(),
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
    result.controllers = records
        .into_iter()
        .map(|r| DomainController {
            host: r.target,
            port: r.port,
            priority: r.priority,
            weight: r.weight,
            reachable: false,
            clock_offset_secs: None,
            in_client_site: false,
            recommended: false,
            warning: None,
        })
        .collect();

    // Client site and its DCs.
    let mut site_hosts = Vec::new();
    if probe {
        if let Some(site) = client_site(runner, &domain) {
            if let Ok(site_records) = dns::lookup_srv(
                &site_srv_name(&domain, &site),
                &nameservers,
                Duration::from_secs(3),
            ) {
                for r in site_records {
                    if !result
                        .controllers
                        .iter()
                        .any(|d| d.host.eq_ignore_ascii_case(&r.target))
                    {
                        result.controllers.push(DomainController {
                            host: r.target.clone(),
                            port: r.port,
                            priority: r.priority,
                            weight: r.weight,
                            reachable: false,
                            clock_offset_secs: None,
                            in_client_site: false,
                            recommended: false,
                            warning: None,
                        });
                    }
                    site_hosts.push(r.target);
                }
            }
            result.client_site = Some(site);
        }
        probe_all(&mut result.controllers);
    }

    result.found = !result.controllers.is_empty();
    if probe {
        let ranking = rank_controllers(&mut result.controllers, &site_hosts);
        result.recommended = ranking.recommended;
        result.majority_clock_offset_secs = ranking.majority_offset;
        result.warnings = ranking.warnings;
        if result.found && result.recommended.is_none() {
            result.problem = Some(format!(
                "Found domain controllers for '{domain}' in DNS, but none of them could be reached. Check the network connection and firewall."
            ));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::FakeRunner;

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
        let res = discover("LAB", &FakeRunner::new(), &Paths::under(dir.path()), false);
        assert!(!res.found);
        assert!(res.problem.unwrap().contains("full domain name"));
    }

    #[test]
    fn reports_dns_problems_in_plain_language() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let rc = paths.resolv_conf();
        fs::create_dir_all(rc.parent().unwrap()).unwrap();
        fs::write(&rc, "nameserver 192.0.2.1\n").unwrap();
        let res = discover("nowhere.invalid", &FakeRunner::new(), &paths, false);
        assert!(!res.found);
        assert!(res
            .problem
            .unwrap()
            .starts_with("Could not look up 'nowhere.invalid'"));
    }

    #[test]
    fn site_name_and_adcli_parsing() {
        assert_eq!(
            site_srv_name("Lab.Test", "Lahore-Campus"),
            "_ldap._tcp.Lahore-Campus._sites.dc._msdcs.lab.test"
        );
        let out = "[domain]\ndomain-name = lab.test\ndomain-controller-site = HQ\n[computer]\ncomputer-site = Lahore-Campus\n";
        assert_eq!(parse_adcli_site(out).as_deref(), Some("Lahore-Campus"));
        assert_eq!(parse_adcli_site("[computer]\ncomputer-site = \n"), None);
        let fake = FakeRunner::new();
        fake.ok("adcli info", out);
        assert_eq!(
            client_site(&fake, "lab.test").as_deref(),
            Some("Lahore-Campus")
        );
        let none = FakeRunner::new();
        none.installed(&[]);
        assert_eq!(client_site(&none, "lab.test"), None);
    }

    fn dc(host: &str, reachable: bool, offset: Option<f64>) -> DomainController {
        DomainController {
            host: host.into(),
            port: 389,
            priority: 0,
            weight: 100,
            reachable,
            clock_offset_secs: offset,
            in_client_site: false,
            recommended: false,
            warning: None,
        }
    }

    /// The lab scenario: 9 DCs, 4 unreachable (stale records), two reachable
    /// DCs with clocks at -162 s and +180 s, three reachable DCs that agree.
    fn lab_scenario() -> Vec<DomainController> {
        vec![
            dc("dc1.lab.test", true, Some(-162.0)),
            dc("dc2.lab.test", false, None),
            dc("dc3.lab.test", true, Some(180.0)),
            dc("dc4.lab.test", false, None),
            dc("dc5.lab.test", true, Some(2.0)),
            dc("dc6.lab.test", false, None),
            dc("dc7.lab.test", true, Some(-5.0)),
            dc("dc8.lab.test", false, None),
            dc("dc9.lab.test", true, Some(1.0)),
        ]
    }

    #[test]
    fn lab_scenario_prefers_site_then_majority_then_smallest_offset() {
        let mut dcs = lab_scenario();
        // The client's site is served by dc7 (reachable) and dc8 (stale).
        let ranking = rank_controllers(&mut dcs, &["DC7.lab.test".into(), "dc8.lab.test".into()]);
        let order: Vec<&str> = dcs.iter().map(|d| d.host.as_str()).collect();
        assert_eq!(
            order,
            [
                "dc7.lab.test", // in site, agrees, |-5|
                "dc9.lab.test", // |1|
                "dc5.lab.test", // |2|
                "dc1.lab.test", // reachable but disagrees (-162)
                "dc3.lab.test", // reachable but disagrees (+180)
                "dc8.lab.test", // unreachable; in site, so first of the unreachable group
                "dc2.lab.test",
                "dc4.lab.test",
                "dc6.lab.test",
            ]
        );
        assert_eq!(ranking.recommended.as_deref(), Some("dc7.lab.test"));
        assert!(dcs[0].recommended && dcs[0].in_client_site);
        assert_eq!(dcs.iter().filter(|d| d.recommended).count(), 1);
        // Majority clock = median of {2, -5, 1} = 1.
        assert_eq!(ranking.majority_offset, Some(1.0));
        // Per-DC warnings in plain language.
        let w = |h: &str| {
            dcs.iter()
                .find(|d| d.host == h)
                .unwrap()
                .warning
                .clone()
                .unwrap_or_default()
        };
        assert!(
            w("dc2.lab.test").contains("stale DNS record"),
            "{}",
            w("dc2.lab.test")
        );
        assert!(
            w("dc1.lab.test").contains("-162 s")
                && w("dc1.lab.test").contains("will not be used to set the time")
        );
        assert!(w("dc3.lab.test").contains("+180 s") && w("dc3.lab.test").contains("disagrees"));
        assert!(dcs
            .iter()
            .find(|d| d.host == "dc7.lab.test")
            .unwrap()
            .warning
            .is_none());
        // Summary warnings.
        assert_eq!(ranking.warnings.len(), 2);
        assert!(ranking.warnings[0].starts_with("4 of 9 domain controllers could not be reached (possibly stale DNS records): dc2.lab.test, dc4.lab.test, dc6.lab.test, dc8.lab.test."));
        assert!(ranking.warnings[1].contains("dc1.lab.test (-162 s), dc3.lab.test (+180 s)"));
        assert!(ranking.warnings[1].ends_with("This computer will use dc7.lab.test for time."));
    }

    #[test]
    fn lab_scenario_without_site_uses_smallest_agreeing_offset() {
        let mut dcs = lab_scenario();
        let ranking = rank_controllers(&mut dcs, &[]);
        assert_eq!(ranking.recommended.as_deref(), Some("dc9.lab.test"));
        let order: Vec<&str> = dcs.iter().take(3).map(|d| d.host.as_str()).collect();
        assert_eq!(order, ["dc9.lab.test", "dc5.lab.test", "dc7.lab.test"]);
        assert!(dcs.iter().all(|d| !d.in_client_site));
    }

    #[test]
    fn majority_is_not_fooled_by_two_bad_clocks() {
        // Two DCs agree at ~+170, one is correct at 0: majority is the pair.
        assert_eq!(majority_offset(&[170.0, 0.0, 175.0]), Some(172.5));
        // Three agree near 0, two far apart: majority near 0.
        assert_eq!(majority_offset(&[-162.0, 180.0, 2.0, -5.0, 1.0]), Some(1.0));
        // Two DCs that disagree with each other: pick the one closer to our clock.
        assert_eq!(majority_offset(&[-162.0, 180.0]), Some(-162.0));
        assert_eq!(majority_offset(&[]), None);
    }

    #[test]
    fn large_but_agreed_offset_means_our_clock_is_wrong() {
        // Every DC says we are 300 s behind: no DC is flagged as disagreeing,
        // but each gets a clock warning, and the majority offset tells join
        // to fix our clock.
        let mut dcs = vec![
            dc("a.lab.test", true, Some(300.0)),
            dc("b.lab.test", true, Some(302.0)),
        ];
        let ranking = rank_controllers(&mut dcs, &[]);
        assert_eq!(ranking.majority_offset, Some(301.0));
        assert!(dcs.iter().all(|d| d
            .warning
            .as_deref()
            .is_some_and(|w| !w.contains("disagrees"))));
        assert_eq!(ranking.recommended.as_deref(), Some("a.lab.test"));
    }

    #[test]
    fn nothing_reachable_means_no_recommendation() {
        let mut dcs = vec![dc("a.lab.test", false, None), dc("b.lab.test", false, None)];
        let ranking = rank_controllers(&mut dcs, &[]);
        assert_eq!(ranking.recommended, None);
        assert!(dcs.iter().all(|d| !d.recommended));
        assert_eq!(ranking.warnings.len(), 1);
    }
}
