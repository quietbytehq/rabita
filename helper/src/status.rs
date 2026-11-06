// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! `Status`: is this computer connected, and is everything healthy?

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

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
    }
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
            joined: false,
            domain: None,
            realm: None,
            computer_name: None,
            domain_controller: None,
            joined_at: None,
            checks,
        };
    }

    // sssd config and the DC it points at.
    let conf_text = std::fs::read_to_string(paths.sssd_conf()).ok();
    let mut dc = state.domain_controller.clone();
    match &conf_text {
        Some(text) => {
            let doc = IniDocument::parse(text);
            let section = domain.as_ref().map(|d| format!("domain/{d}"));
            let fq = section
                .as_ref()
                .and_then(|s| doc.get(s, "use_fully_qualified_names"))
                .unwrap_or_else(|| "True".to_owned());
            if let Some(server) = section.as_ref().and_then(|s| doc.get(s, "ad_server")) {
                dc = Some(server);
            }
            checks.push(check(
                "sssd-config",
                "Sign-in configuration",
                Some(true),
                if fq.eq_ignore_ascii_case("false") {
                    "Plain usernames enabled"
                } else {
                    "Present (users must sign in as user@domain)"
                },
            ));
        }
        None => checks.push(check(
            "sssd-config",
            "Sign-in configuration",
            Some(false),
            "/etc/sssd/sssd.conf is missing",
        )),
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

    // DC reachability
    if probe {
        let target = dc.clone().or_else(|| {
            domain
                .as_ref()
                .map(|d| crate::discover::discover(d, paths, true))
                .and_then(|r| {
                    r.controllers
                        .into_iter()
                        .find(|c| c.reachable)
                        .map(|c| c.host)
                })
        });
        match target {
            Some(host) => {
                let reachable = format!("{host}:389").to_socket_addrs().is_ok_and(|mut a| {
                    a.any(|a| TcpStream::connect_timeout(&a, Duration::from_secs(3)).is_ok())
                });
                checks.push(check(
                    "dc",
                    "Domain controller",
                    Some(reachable),
                    if reachable {
                        format!("{host} is reachable")
                    } else {
                        format!("{host} could not be reached")
                    },
                ));
                if dc.is_none() {
                    dc = Some(host);
                }
            }
            None => checks.push(check(
                "dc",
                "Domain controller",
                Some(false),
                "No domain controller found",
            )),
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
        write("/etc/sssd/sssd.conf", "[sssd]\n[domain/lab.test]\nuse_fully_qualified_names = False\nad_server = dc1.lab.test\n");
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
                "sssd-config",
                "sssd-service",
                "keytab",
                "kerberos",
                "time",
                "mkhomedir",
                "user-lookup"
            ]
        );
    }
}
