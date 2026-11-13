// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! `Status`: is this computer connected, and is everything healthy?

use crate::backup::Store;
use crate::cmd::{CommandSpec, Runner};
use crate::ini::IniDocument;
use crate::join::keytab_principal;
use crate::paths::Paths;
use crate::system;
use crate::types::{HealthCheck, StatusReport};

fn check(id: &str, label: &str, ok: Option<bool>, detail: impl Into<String>) -> HealthCheck {
    HealthCheck {
        id: id.to_owned(),
        label: label.to_owned(),
        ok,
        detail: detail.into(),
        warning: false,
    }
}

/// Builds the domain-controller checks from a discovery result: whether the
/// configured DC (or the best one) is reachable, plus warnings about stale
/// records and bad clocks. Pure, so the lab scenario can be tested.
#[must_use]
pub fn dc_checks(
    disc: &crate::types::DiscoveryResult,
    configured: Option<&str>,
) -> Vec<HealthCheck> {
    let mut checks = Vec::new();
    let configured_dc = configured.and_then(|h| {
        disc.controllers
            .iter()
            .find(|c| c.host.eq_ignore_ascii_case(h))
    });
    let main = match (configured_dc, disc.recommended.as_deref()) {
        (Some(c), _) if c.reachable => check(
            "dc",
            "Domain controller",
            Some(true),
            format!("{} is reachable", c.host),
        ),
        (Some(c), Some(best)) => {
            let mut hc = check(
                "dc",
                "Domain controller",
                Some(true),
                format!("{} could not be reached; using {best} instead", c.host),
            );
            hc.warning = true;
            hc
        }
        (Some(c), None) => check(
            "dc",
            "Domain controller",
            Some(false),
            format!("{} could not be reached", c.host),
        ),
        (None, Some(best)) => check(
            "dc",
            "Domain controller",
            Some(true),
            format!("{best} is reachable"),
        ),
        (None, None) => check(
            "dc",
            "Domain controller",
            Some(false),
            disc.problem
                .clone()
                .unwrap_or_else(|| "No domain controller could be reached".to_owned()),
        ),
    };
    checks.push(main);
    let reachable = disc.controllers.iter().filter(|c| c.reachable).count();
    let mut health = check(
        "dc-health",
        "Domain controller health",
        Some(true),
        if disc.warnings.is_empty() {
            format!(
                "All {} domain controllers are reachable with good clocks",
                disc.controllers.len()
            )
        } else {
            format!(
                "{reachable} of {} reachable. {}",
                disc.controllers.len(),
                disc.warnings.join(" ")
            )
        },
    );
    health.warning = !disc.warnings.is_empty();
    checks.push(health);
    checks
}

/// Formats a Unix timestamp as RFC 3339 (UTC) without external crates.
#[must_use]
pub fn format_rfc3339(secs: u64) -> String {
    // Civil-from-days algorithm (Howard Hinnant), valid for the Unix era.
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// The first real server in an `ad_server` list (`dc1, _srv_` -> `dc1`).
#[must_use]
pub fn first_ad_server(value: &str) -> Option<String> {
    value
        .split(',')
        .map(str::trim)
        .find(|s| !s.is_empty() && *s != "_srv_")
        .map(str::to_owned)
}

/// Whether the steps after joining all completed.
#[must_use]
pub fn setup_check(state: &crate::backup::State) -> HealthCheck {
    match state.setup_finished {
        Some(true) => check("setup", "Setup", Some(true), "Finished"),
        Some(false) => {
            let labels: Vec<&str> = state
                .remaining_steps
                .iter()
                .map(|id| crate::join::step_label(id))
                .collect();
            check(
                "setup",
                "Setup",
                Some(false),
                format!(
                    "Joined but not finished. Still to do: {}. Run Finish setup (rabitactl finish).",
                    labels.join(", ")
                ),
            )
        }
        None => {
            let mut hc = check(
                "setup",
                "Setup",
                Some(true),
                "Joined by an earlier version of Rabita. Run Finish setup (rabitactl finish) once to bring it up to date.",
            );
            hc.warning = true;
            hc
        }
    }
}

/// Reads who may sign in from `sssd.conf`, as `realm permit` wrote it.
#[must_use]
pub fn describe_login_access(doc: &IniDocument, section: Option<&str>) -> String {
    let Some(section) = section else {
        return "All domain users".to_owned();
    };
    let provider = doc
        .get(section, "access_provider")
        .unwrap_or_else(|| "ad".to_owned())
        .to_ascii_lowercase();
    if provider != "simple" {
        return "All domain users".to_owned();
    }
    let groups = doc.get(section, "simple_allow_groups").unwrap_or_default();
    let users = doc.get(section, "simple_allow_users").unwrap_or_default();
    let groups: Vec<&str> = groups
        .split(',')
        .map(str::trim)
        .filter(|g| !g.is_empty())
        .collect();
    let users: Vec<&str> = users
        .split(',')
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .collect();
    match (groups.is_empty(), users.is_empty()) {
        (true, true) => "Nobody (no groups or users permitted)".to_owned(),
        (false, true) => format!("Only these groups: {}", groups.join(", ")),
        (true, false) => format!("Only these users: {}", users.join(", ")),
        (false, false) => format!(
            "Only these groups: {} (and users: {})",
            groups.join(", "),
            users.join(", ")
        ),
    }
}

/// Builds the status report. `probe` enables network checks.
#[must_use]
#[allow(clippy::too_many_lines)] // one check after another
pub fn report(runner: &dyn Runner, paths: &Paths, probe: bool) -> StatusReport {
    let state = Store::new(paths.clone()).load().unwrap_or_default();
    let realm_entry = system::joined_realm(runner);
    let joined = realm_entry.is_some() || state.joined;
    let domain = realm_entry
        .as_ref()
        .and_then(|e| e.domain_name.clone().or(Some(e.name.clone())))
        .or_else(|| state.domain.clone());
    let realm = state
        .realm
        .clone()
        .or_else(|| domain.as_ref().map(|d| d.to_ascii_uppercase()));

    let mut checks = Vec::new();
    checks.push(match (&realm_entry, state.joined) {
        (Some(e), _) => check(
            "membership",
            "Domain membership",
            Some(true),
            format!(
                "Member of {}",
                e.domain_name.clone().unwrap_or(e.name.clone())
            ),
        ),
        (None, true) => check(
            "membership",
            "Domain membership",
            Some(false),
            "Rabita joined a domain, but the system no longer lists it.",
        ),
        (None, false) => check(
            "membership",
            "Domain membership",
            Some(false),
            "Not connected to a domain.",
        ),
    });

    if !joined {
        return StatusReport {
            checks,
            ..StatusReport::default()
        };
    }

    if state.joined {
        checks.push(setup_check(&state));
    }

    // sssd config and the DC it points at.
    let conf_text = std::fs::read_to_string(paths.sssd_conf()).ok();
    let mut dc = state.domain_controller.clone();
    let mut login_access_detail = None;
    match &conf_text {
        Some(text) => {
            let doc = IniDocument::parse(text);
            let section = domain.as_ref().map(|d| format!("domain/{d}"));
            let fq = section
                .as_ref()
                .and_then(|s| doc.get(s, "use_fully_qualified_names"))
                .unwrap_or_else(|| "True".to_owned());
            if let Some(server) = section
                .as_ref()
                .and_then(|s| doc.get(s, "ad_server"))
                .and_then(|v| first_ad_server(&v))
            {
                dc = Some(server);
            }
            let needs_repair = crate::join::sssd_conf_needs_repair(text);
            login_access_detail = Some(describe_login_access(&doc, section.as_deref()));
            checks.push(if needs_repair {
                check(
                    "sssd-config",
                    "Sign-in configuration",
                    Some(false),
                    "sssd.conf sets default_domain_suffix, which stops SSSD 2.10 and newer from starting. Run Finish setup (rabitactl finish) to repair it.",
                )
            } else {
                check(
                    "sssd-config",
                    "Sign-in configuration",
                    Some(true),
                    if fq.eq_ignore_ascii_case("false") {
                        "Plain usernames enabled"
                    } else {
                        "Present (users must sign in as user@domain)"
                    },
                )
            });
        }
        None => checks.push(check(
            "sssd-config",
            "Sign-in configuration",
            Some(false),
            "/etc/sssd/sssd.conf is missing",
        )),
    }

    if let Some(detail) = login_access_detail {
        checks.push(check("login-access", "Who can sign in", Some(true), detail));
    }

    // sssd service
    let active = runner
        .run(&CommandSpec::new("systemctl", &["is-active", "sssd"]))
        .map_or_else(|e| e.to_string(), |o| o.stdout.trim().to_owned());
    checks.push(check(
        "sssd-service",
        "Sign-in service",
        Some(active == "active"),
        if active == "active" {
            "Running".to_owned()
        } else {
            format!("Not running ({active})")
        },
    ));

    // keytab + kerberos
    let keytab_ok = paths.keytab().is_file();
    checks.push(check(
        "keytab",
        "Computer account key",
        Some(keytab_ok),
        if keytab_ok {
            "Present"
        } else {
            "/etc/krb5.keytab is missing"
        },
    ));
    if keytab_ok && runner.which("kinit") {
        let klist = runner
            .run(&CommandSpec::new("klist", &["-k", "/etc/krb5.keytab"]))
            .map(|o| o.stdout)
            .unwrap_or_default();
        match keytab_principal(&klist) {
            Some(principal) => {
                let out = runner.run(&CommandSpec::new(
                    "kinit",
                    &[
                        "-k",
                        "-t",
                        "/etc/krb5.keytab",
                        "-c",
                        "MEMORY:rabita-status",
                        &principal,
                    ],
                ));
                let (ok, detail) = match out {
                    Ok(o) if o.success() => (true, format!("Ticket obtained as {principal}")),
                    Ok(o) => (
                        false,
                        o.combined()
                            .lines()
                            .last()
                            .unwrap_or("kinit failed")
                            .to_owned(),
                    ),
                    Err(e) => (false, e.to_string()),
                };
                checks.push(check("kerberos", "Kerberos sign-in", Some(ok), detail));
            }
            None => checks.push(check(
                "kerberos",
                "Kerberos sign-in",
                Some(false),
                "No computer account key found",
            )),
        }
    } else {
        checks.push(check("kerberos", "Kerberos sign-in", None, "Not checked"));
    }

    // DC reachability and health (stale records, clocks)
    if probe {
        if let Some(d) = &domain {
            let disc = crate::discover::discover(d, runner, paths, true);
            checks.extend(dc_checks(&disc, dc.as_deref()));
            if dc.is_none() {
                dc = disc.recommended;
            }
        }
    }

    // time sync
    if runner.which("timedatectl") {
        let out = runner
            .run(&CommandSpec::new(
                "timedatectl",
                &["show", "-p", "NTPSynchronized", "--value"],
            ))
            .map(|o| o.stdout.trim().to_owned())
            .unwrap_or_default();
        checks.push(check(
            "time",
            "Clock",
            Some(out == "yes"),
            if out == "yes" {
                "Synchronized"
            } else {
                "Not synchronized (Kerberos needs accurate time)"
            },
        ));
    }

    // home dirs
    let pam = std::fs::read_to_string(paths.pam_common_session()).unwrap_or_default();
    checks.push(check(
        "mkhomedir",
        "Home folders on first login",
        Some(pam.contains("pam_mkhomedir.so")),
        if pam.contains("pam_mkhomedir.so") {
            "Enabled"
        } else {
            "Not enabled"
        },
    ));

    // user lookup
    if let Some(user) = state.verify_user.as_deref().filter(|u| !u.is_empty()) {
        let out = runner.run(&CommandSpec::new("getent", &["passwd", user]));
        let ok = out
            .as_ref()
            .is_ok_and(|o| o.success() && !o.stdout.trim().is_empty());
        checks.push(check(
            "user-lookup",
            "Domain user lookup",
            Some(ok),
            if ok {
                format!("'{user}' resolves")
            } else {
                format!("'{user}' could not be looked up")
            },
        ));
    }

    StatusReport {
        joined: true,
        domain,
        realm,
        computer_name: state.computer_name.clone(),
        domain_controller: dc,
        joined_at: state.joined_at.map(format_rfc3339),
        setup_finished: state.setup_finished.filter(|_| state.joined),
        remaining_steps: state.remaining_steps.clone(),
        options: state.joined.then(|| state.options.clone()),
        checks,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::backup::State;
    use crate::cmd::FakeRunner;

    #[test]
    fn formats_timestamps() {
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn not_joined_report_is_short() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new();
        fake.ok("realm list", "");
        let rep = report(&fake, &Paths::under(dir.path()), false);
        assert!(!rep.joined);
        assert_eq!(rep.checks.len(), 1);
        assert_eq!(rep.checks[0].ok, Some(false));
    }

    #[test]
    fn joined_report_runs_health_checks() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let write = |abs: &str, content: &str| {
            let p = paths.resolve(Path::new(abs));
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, content).unwrap();
        };
        write("/etc/sssd/sssd.conf", "[sssd]\n[domain/lab.test]\nuse_fully_qualified_names = False\nad_server = dc1.lab.test, _srv_\n");
        write("/etc/krb5.keytab", "x");
        write(
            "/etc/pam.d/common-session",
            "session optional pam_mkhomedir.so\n",
        );
        Store::new(paths.clone())
            .save(&State {
                domain: Some("lab.test".into()),
                realm: Some("LAB.TEST".into()),
                joined: true,
                joined_at: Some(1_700_000_000),
                computer_name: Some("PC1".into()),
                verify_user: Some("amna".into()),
                setup_finished: Some(true),
                ..State::default()
            })
            .unwrap();
        let fake = FakeRunner::new();
        fake.ok(
            "realm list",
            "lab.test\n  domain-name: lab.test\n  configured: kerberos-member\n",
        )
        .ok("systemctl is-active", "active\n")
        .ok("klist", "   2 PC1$@LAB.TEST\n")
        .ok("kinit", "")
        .ok("timedatectl", "yes\n")
        .ok("getent", "amna:*:1:1::/home/amna:/bin/bash\n");
        let rep = report(&fake, &paths, false);
        assert!(rep.joined);
        assert_eq!(rep.domain.as_deref(), Some("lab.test"));
        assert_eq!(rep.domain_controller.as_deref(), Some("dc1.lab.test"));
        assert_eq!(rep.joined_at.as_deref(), Some("2023-11-14T22:13:20Z"));
        let failing: Vec<&str> = rep
            .checks
            .iter()
            .filter(|c| c.ok == Some(false))
            .map(|c| c.id.as_str())
            .collect();
        assert!(failing.is_empty(), "failing checks: {failing:?}");
        let ids: Vec<&str> = rep.checks.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "membership",
                "setup",
                "sssd-config",
                "login-access",
                "sssd-service",
                "keytab",
                "kerberos",
                "time",
                "mkhomedir",
                "user-lookup"
            ]
        );
        assert_eq!(rep.checks[3].detail, "All domain users");
        assert_eq!(rep.setup_finished, Some(true));
        assert_eq!(rep.options, Some(crate::types::JoinOptions::default()));
    }

    #[test]
    fn reports_joined_but_not_finished_and_the_bad_sssd_setting() {
        let mut state = State {
            joined: true,
            setup_finished: Some(false),
            remaining_steps: vec!["home".into(), "verify".into()],
            ..State::default()
        };
        let hc = setup_check(&state);
        assert_eq!(hc.ok, Some(false));
        assert_eq!(
            hc.detail,
            "Joined but not finished. Still to do: Create home folders on first login, Check that it works. Run Finish setup (rabitactl finish)."
        );
        state.setup_finished = None;
        let hc = setup_check(&state);
        assert_eq!(hc.ok, Some(true));
        assert!(hc.warning && hc.detail.contains("earlier version"));
        state.setup_finished = Some(true);
        assert_eq!(setup_check(&state).detail, "Finished");

        assert_eq!(
            first_ad_server("dc1.lab.test, _srv_").as_deref(),
            Some("dc1.lab.test")
        );
        assert_eq!(
            first_ad_server("_srv_, dc2.lab.test").as_deref(),
            Some("dc2.lab.test")
        );
        assert_eq!(first_ad_server("_srv_"), None);

        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let conf = paths.sssd_conf();
        fs::create_dir_all(conf.parent().unwrap()).unwrap();
        fs::write(&conf, "[sssd]\ndefault_domain_suffix = lab.test\n[domain/lab.test]\nuse_fully_qualified_names = False\n").unwrap();
        Store::new(paths.clone())
            .save(&State {
                domain: Some("lab.test".into()),
                joined: true,
                setup_finished: Some(false),
                remaining_steps: vec!["sssd".into()],
                ..State::default()
            })
            .unwrap();
        let fake = FakeRunner::new();
        fake.ok(
            "realm list",
            "lab.test\n  domain-name: lab.test\n  configured: kerberos-member\n",
        );
        let rep = report(&fake, &paths, false);
        assert_eq!(rep.setup_finished, Some(false));
        assert_eq!(rep.remaining_steps, ["sssd"]);
        let get = |id: &str| rep.checks.iter().find(|c| c.id == id).unwrap().clone();
        assert_eq!(get("setup").ok, Some(false));
        assert_eq!(get("sssd-config").ok, Some(false));
        assert!(get("sssd-config").detail.contains("stops SSSD 2.10"));
    }

    #[test]
    fn dc_checks_flag_stale_records_and_bad_clocks() {
        use crate::discover::rank_controllers;
        use crate::types::{DiscoveryResult, DomainController};
        let dc = |host: &str, reachable: bool, offset: Option<f64>| DomainController {
            host: host.into(),
            port: 389,
            priority: 0,
            weight: 100,
            reachable,
            clock_offset_secs: offset,
            in_client_site: false,
            recommended: false,
            warning: None,
        };
        let mut controllers = vec![
            dc("dc1.lab.test", true, Some(-162.0)),
            dc("dc2.lab.test", false, None),
            dc("dc3.lab.test", true, Some(180.0)),
            dc("dc4.lab.test", false, None),
            dc("dc5.lab.test", true, Some(2.0)),
            dc("dc6.lab.test", false, None),
            dc("dc7.lab.test", true, Some(-5.0)),
            dc("dc8.lab.test", false, None),
            dc("dc9.lab.test", true, Some(1.0)),
        ];
        let ranking = rank_controllers(&mut controllers, &[]);
        let disc = DiscoveryResult {
            domain: "lab.test".into(),
            realm: "LAB.TEST".into(),
            found: true,
            controllers,
            problem: None,
            client_site: None,
            recommended: ranking.recommended,
            majority_clock_offset_secs: ranking.majority_offset,
            warnings: ranking.warnings,
        };
        // Configured DC is one of the stale ones.
        let checks = dc_checks(&disc, Some("dc2.lab.test"));
        assert_eq!(checks[0].ok, Some(true));
        assert!(checks[0].warning);
        assert_eq!(
            checks[0].detail,
            "dc2.lab.test could not be reached; using dc9.lab.test instead"
        );
        assert_eq!(checks[1].id, "dc-health");
        assert!(checks[1].warning);
        assert!(checks[1].detail.starts_with(
            "5 of 9 reachable. 4 of 9 domain controllers could not be reached (possibly stale DNS records)"
        ));
        assert!(checks[1]
            .detail
            .contains("dc1.lab.test (-162 s), dc3.lab.test (+180 s)"));
        // Configured DC healthy.
        let checks = dc_checks(&disc, Some("dc7.lab.test"));
        assert_eq!(checks[0].detail, "dc7.lab.test is reachable");
        assert!(!checks[0].warning);
    }

    #[test]
    fn describes_login_access_from_sssd_conf() {
        let all = IniDocument::parse("[domain/x]\naccess_provider = ad\n");
        assert_eq!(
            describe_login_access(&all, Some("domain/x")),
            "All domain users"
        );
        let groups = IniDocument::parse(
            "[domain/x]\naccess_provider = simple\nsimple_allow_groups = Linux Users, Staff\n",
        );
        assert_eq!(
            describe_login_access(&groups, Some("domain/x")),
            "Only these groups: Linux Users, Staff"
        );
        let nobody = IniDocument::parse("[domain/x]\naccess_provider = simple\n");
        assert!(describe_login_access(&nobody, Some("domain/x")).starts_with("Nobody"));
        assert_eq!(describe_login_access(&all, None), "All domain users");
    }
}
