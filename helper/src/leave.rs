// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! The `Leave` job: disconnect from the organization and put every file
//! back the way it was.

use std::time::Duration;

use rabita_backends::{DistroFamily, SystemFacts};
use zeroize::Zeroizing;

use crate::backup::Store;
use crate::cmd::CommandSpec;
use crate::join::{execute_actions, JobContext, StepError};
use crate::progress::{Collector, Reporter, Tee};
use crate::system;
use crate::types::{JobResult, LeaveRequest, StepState};

const LEAVE_STEPS: [(&str, &str); 5] = [
    ("leave", "Leave the domain"),
    ("restore", "Restore the original settings"),
    ("login", "Turn off domain sign-in rules"),
    ("display", "Restore the login screen"),
    ("finish", "Clean up"),
];

/// Runs the whole leave and returns the result.
#[must_use]
pub fn run_leave(
    ctx: &JobContext<'_>,
    req: &LeaveRequest,
    password: Zeroizing<String>,
) -> JobResult {
    let collector = Collector::new();
    let tee = Tee(vec![ctx.sink, &collector]);
    let reporter = Reporter::new(&tee);
    for (id, label) in LEAVE_STEPS {
        reporter.step(id, label, StepState::Pending, "");
    }
    let outcome = leave_inner(ctx, req, password, &reporter);
    let mut steps = collector.outcomes();
    if outcome.is_err() {
        for s in &mut steps {
            if s.state == StepState::Pending {
                s.state = StepState::Skipped;
                "Not run because an earlier step failed.".clone_into(&mut s.message);
            }
        }
    }
    JobResult {
        ok: outcome.is_ok(),
        summary: match outcome {
            Ok(domain) => match domain {
                Some(d) => format!("This computer has left {d}. Original settings are restored."),
                None => "This computer was not connected to a domain. Nothing to undo.".to_owned(),
            },
            Err(e) => e.0,
        },
        steps,
    }
}

fn step<T>(
    r: &Reporter<'_>,
    idx: usize,
    f: impl FnOnce(&Reporter<'_>) -> Result<(StepState, String, T), StepError>,
) -> Result<T, StepError> {
    let (id, label) = LEAVE_STEPS[idx];
    r.step(id, label, StepState::Running, "");
    match f(r) {
        Ok((state, msg, v)) => {
            r.step(id, label, state, &msg);
            Ok(v)
        }
        Err(e) => {
            r.step(id, label, StepState::Failed, &e.0);
            Err(e)
        }
    }
}

#[allow(clippy::too_many_lines)] // one step after another; splitting would hide the order
fn leave_inner(
    ctx: &JobContext<'_>,
    req: &LeaveRequest,
    password: Zeroizing<String>,
    r: &Reporter<'_>,
) -> Result<Option<String>, StepError> {
    let store = Store::new(ctx.paths.clone());
    let mut state = store
        .load()
        .map_err(|e| StepError(format!("Could not read Rabita's state: {e}")))?;
    let realm_entry = system::joined_realm(ctx.runner);
    let domain = state.domain.clone().or_else(|| {
        realm_entry
            .as_ref()
            .and_then(|e| e.domain_name.clone().or(Some(e.name.clone())))
    });

    // 1. realm leave
    step(r, 0, |r| {
        let Some(domain) = &domain else {
            return Ok((StepState::Skipped, "Not joined to a domain.".to_owned(), ()));
        };
        if realm_entry.is_none() {
            return Ok((
                StepState::Skipped,
                "The system no longer lists a domain membership.".to_owned(),
                (),
            ));
        }
        let mut args = vec![
            "leave".to_owned(),
            "--unattended".to_owned(),
            "--verbose".to_owned(),
        ];
        let mut spec_stdin = None;
        if req.remove_computer_account {
            let user = req
                .admin_user
                .as_deref()
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .ok_or("Enter the administrator account to delete the computer account.")?;
            args.push("--remove".to_owned());
            args.push(format!("--user={user}"));
            let mut secret = Zeroizing::new(String::with_capacity(password.len() + 1));
            secret.push_str(&password);
            secret.push('\n');
            spec_stdin = Some(secret);
        }
        args.push(domain.clone());
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut spec = CommandSpec::new("realm", &arg_refs).timeout(Duration::from_secs(5 * 60));
        if let Some(s) = spec_stdin {
            spec = spec.stdin(s);
        }
        let display = spec.display();
        let out = ctx
            .runner
            .run(&spec)
            .map_err(|e| StepError(format!("Could not run realm: {e}")))?;
        r.log_command(&display, &out);
        if !out.success() {
            return Err(StepError(format!(
                "Leaving the domain failed: {}",
                out.combined().lines().last().unwrap_or("unknown error")
            )));
        }
        Ok((StepState::Done, format!("Left {domain}."), ()))
    })?;
    drop(password);

    // 2. restore files
    step(r, 1, |r| {
        let notes = store
            .restore_all(&state)
            .map_err(|e| StepError(format!("Restoring files failed: {e}")))?;
        for n in &notes {
            r.log(n.clone());
        }
        if notes.is_empty() {
            Ok((StepState::Skipped, "No files to restore.".to_owned(), ()))
        } else {
            Ok((
                StepState::Done,
                format!("Restored {} file(s).", notes.len()),
                (),
            ))
        }
    })?;

    let info = system::detect(ctx.runner, ctx.paths);
    let backend = info.family.unwrap_or(DistroFamily::Debian).backend();
    let facts = SystemFacts {
        mkhomedir_already_enabled: state.mkhomedir_was_enabled,
        ..system::gather_facts(ctx.paths)
    };

    // 3. login integration
    step(r, 2, |r| {
        if !state.joined && state.backups.is_empty() {
            return Ok((StepState::Skipped, "Nothing was changed.".to_owned(), ()));
        }
        let actions = backend.disable_login_integration(&facts);
        if actions.is_empty() {
            return Ok((StepState::Skipped, "Nothing to undo.".to_owned(), ()));
        }
        execute_actions(
            ctx,
            &store,
            &mut state,
            r,
            &actions,
            Duration::from_secs(60),
        )?;
        Ok((StepState::Done, "Done.".to_owned(), ()))
    })?;

    // 4. display manager
    step(r, 3, |r| {
        let actions = backend.disable_manual_login(&facts);
        if actions.is_empty() {
            return Ok((StepState::Skipped, "Nothing to undo.".to_owned(), ()));
        }
        execute_actions(
            ctx,
            &store,
            &mut state,
            r,
            &actions,
            Duration::from_secs(60),
        )?;
        Ok((StepState::Done, "Done.".to_owned(), ()))
    })?;

    // 5. clean up
    step(r, 4, |r| {
        if ctx.runner.which("systemctl") {
            let out = ctx
                .runner
                .run(&CommandSpec::new("systemctl", &["try-restart", "sssd"]))
                .map_err(|e| StepError(e.to_string()))?;
            r.log_command("systemctl try-restart sssd", &out);
        }
        store
            .clear()
            .map_err(|e| StepError(format!("Could not clear Rabita's state: {e}")))?;
        Ok((StepState::Done, "Done.".to_owned(), ()))
    })?;
    Ok(domain)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::backup::State;
    use crate::cmd::FakeRunner;
    use crate::paths::Paths;

    #[test]
    fn leave_restores_files_and_clears_state() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let store = Store::new(paths.clone());
        let write = |abs: &str, content: &str| {
            let p = paths.resolve(Path::new(abs));
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, content).unwrap();
        };
        write(
            "/etc/os-release",
            "ID=ubuntu\nID_LIKE=debian\nPRETTY_NAME=Ubuntu\n",
        );
        write(
            "/etc/pam.d/common-session",
            "session required pam_unix.so\n",
        );

        // A previous join: sssd.conf did not exist, lightdm drop-in created,
        // krb5.conf existed and was changed.
        write("/etc/krb5.conf", "[libdefaults]\n# original\n");
        let mut state = State {
            domain: Some("lab.quietbyte.test".into()),
            joined: true,
            ..State::default()
        };
        store
            .write_file(
                &mut state,
                Path::new("/etc/krb5.conf"),
                "[libdefaults]\n# changed\n",
                0o644,
            )
            .unwrap();
        store
            .write_file(
                &mut state,
                Path::new("/etc/sssd/sssd.conf"),
                "[sssd]\n",
                0o600,
            )
            .unwrap();
        store
            .write_file(
                &mut state,
                Path::new("/etc/lightdm/lightdm.conf.d/90-rabita.conf"),
                "[Seat:*]\n",
                0o644,
            )
            .unwrap();
        store.save(&state).unwrap();

        let fake = FakeRunner::new();
        fake.ok("realm list", "lab.quietbyte.test\n  domain-name: lab.quietbyte.test\n  configured: kerberos-member\n")
            .ok("realm leave", "");
        let collector = Collector::new();
        let ctx = JobContext {
            runner: &fake,
            paths: &paths,
            sink: &collector,
            network: false,
        };
        let result = run_leave(
            &ctx,
            &LeaveRequest::default(),
            Zeroizing::new(String::new()),
        );
        assert!(result.ok, "{}", result.summary);
        assert_eq!(
            result.summary,
            "This computer has left lab.quietbyte.test. Original settings are restored."
        );

        assert_eq!(
            fs::read_to_string(paths.resolve(Path::new("/etc/krb5.conf"))).unwrap(),
            "[libdefaults]\n# original\n"
        );
        assert!(!paths.sssd_conf().exists());
        assert!(!paths
            .resolve(Path::new("/etc/lightdm/lightdm.conf.d/90-rabita.conf"))
            .exists());
        assert!(!store.exists());
        let lines = fake.call_lines();
        assert!(lines.contains(&"realm leave --unattended --verbose lab.quietbyte.test".to_owned()));
        assert!(lines
            .iter()
            .any(|l| l.starts_with("pam-auth-update --package --remove")));
        // No password was supplied, so nothing went to stdin.
        assert!(fake.calls().iter().all(|c| c.stdin.is_none()));
    }

    #[test]
    fn leave_with_remove_passes_password_on_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let p = paths.os_release();
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "ID=ubuntu\n").unwrap();
        Store::new(paths.clone())
            .save(&State {
                domain: Some("lab.test".into()),
                joined: true,
                ..State::default()
            })
            .unwrap();
        let fake = FakeRunner::new();
        fake.ok("realm list", "lab.test\n  configured: kerberos-member\n");
        let collector = Collector::new();
        let ctx = JobContext {
            runner: &fake,
            paths: &paths,
            sink: &collector,
            network: false,
        };
        let req = LeaveRequest {
            remove_computer_account: true,
            admin_user: Some("joiner".into()),
        };
        let result = run_leave(&ctx, &req, Zeroizing::new("pw".into()));
        assert!(result.ok, "{}", result.summary);
        let leave = fake
            .calls()
            .into_iter()
            .find(|c| c.args.first().is_some_and(|a| a == "leave"))
            .unwrap();
        assert!(leave.args.contains(&"--remove".to_owned()));
        assert!(leave.args.contains(&"--user=joiner".to_owned()));
        assert!(leave.args.iter().all(|a| !a.contains("pw")));
        assert_eq!(leave.stdin.as_deref().map(String::as_str), Some("pw\n"));
    }

    #[test]
    fn leave_when_not_joined_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let p = paths.os_release();
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "ID=ubuntu\n").unwrap();
        let fake = FakeRunner::new();
        fake.ok("realm list", "");
        let collector = Collector::new();
        let ctx = JobContext {
            runner: &fake,
            paths: &paths,
            sink: &collector,
            network: false,
        };
        let result = run_leave(
            &ctx,
            &LeaveRequest::default(),
            Zeroizing::new(String::new()),
        );
        assert!(result.ok);
        assert!(result.summary.contains("Nothing to undo"));
        assert!(!fake
            .call_lines()
            .iter()
            .any(|l| l.starts_with("realm leave")));
    }
}
