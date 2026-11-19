// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! The `Leave` job: disconnect from the organization and put back every
//! setting Rabita changed.
//!
//! 1. leave    — `realm leave`; with an admin account also `--remove` the
//!    computer account from the directory. If the directory can't be
//!    reached, fall back to leaving on this computer only and say that the
//!    account must be deleted by IT. A wrong password or missing rights stop
//!    here, before anything changes.
//! 2. restore  — put back every backed-up file (sssd, Kerberos, PAM,
//!    `/etc/hostname`, `/etc/hosts`, time sync)
//! 3. login    — turn off the home-folder login rule Rabita added
//! 4. display  — take the username box off the login screen
//! 5. dns      — put back the DNS setting Rabita changed
//! 6. name     — put back the old computer name
//! 7. finish   — restart sign-in, forget Rabita's records
//!
//! Steps 2–6 are best effort: one that fails does not stop the others. If
//! any fails, Rabita keeps its records so the next Leave (or "Undo changes"
//! on Welcome) can finish the job.
//!
//! The same job undoes changes from a join that never completed (DNS server
//! or computer name changed, but not joined): step 1 is then skipped.

use std::time::Duration;

use rabita_backends::{Action, DistroFamily, SystemFacts};
use zeroize::Zeroizing;

use crate::backup::Store;
use crate::cmd::CommandSpec;
use crate::join::{execute_actions, JobContext, StepError};
use crate::netconf;
use crate::progress::{Collector, Reporter, Tee};
use crate::system;
use crate::types::{JobResult, LeaveRequest, StepState};

const LEAVE_STEPS: [(&str, &str); 7] = [
    ("leave", "Leave the domain"),
    ("restore", "Restore the original settings"),
    ("login", "Turn off domain sign-in rules"),
    ("display", "Restore the login screen"),
    ("dns", "Restore the DNS server"),
    ("name", "Restore the computer name"),
    ("finish", "Clean up"),
];

/// What happened, for the summary.
#[derive(Debug, Default)]
struct Outcome {
    domain: Option<String>,
    computer_name: Option<String>,
    /// `realm leave` ran (the computer was a member).
    left: bool,
    /// The computer account is still in the directory.
    left_locally_only: bool,
    /// Something was put back (files, DNS, name).
    restored: bool,
    /// Plain descriptions of what could not be put back.
    problems: Vec<String>,
}

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
    match outcome {
        Err(e) => JobResult {
            summary: e.0,
            steps,
            ..JobResult::default()
        },
        Ok(o) => JobResult {
            ok: o.problems.is_empty(),
            summary: summary(&o),
            steps,
            left_locally_only: o.left && o.left_locally_only,
            ..JobResult::default()
        },
    }
}

fn summary(o: &Outcome) -> String {
    let domain = o.domain.as_deref().unwrap_or("the domain");
    if !o.problems.is_empty() {
        let start = if o.left {
            format!("This computer has left {domain}, but some settings could not be put back")
        } else {
            "Some settings could not be put back".to_owned()
        };
        return format!(
            "{start}: {}. Open Rabita Setup again to try once more.",
            o.problems.join("; ")
        );
    }
    if !o.left {
        return if o.restored {
            "Rabita put back the settings it had changed.".to_owned()
        } else {
            "This computer was not connected to a domain. Nothing to undo.".to_owned()
        };
    }
    if o.left_locally_only {
        let account = o.computer_name.as_deref().map_or_else(
            || "Its computer account".to_owned(),
            |n| format!("Its computer account ({n})"),
        );
        format!(
            "This computer has left {domain} and its original settings are restored. {account} is still in the directory: ask your IT admin to delete it in Active Directory Users and Computers."
        )
    } else {
        format!(
            "This computer has left {domain}, its computer account was removed, and its original settings are restored."
        )
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

/// A wrong password or missing rights: the person can fix it, so Leave
/// stops instead of falling back.
#[must_use]
pub fn credential_problem(output: &str) -> Option<String> {
    let lower = output.to_ascii_lowercase();
    if lower.contains("couldn't authenticate")
        || lower.contains("preauthentication failed")
        || lower.contains("password incorrect")
        || lower.contains("client not found in kerberos database")
    {
        return Some("The administrator username or password was not accepted.".to_owned());
    }
    if lower.contains("insufficient permissions")
        || lower.contains("insufficient access")
        || lower.contains("permission denied")
        || lower.contains("access denied")
    {
        return Some("That account is not allowed to remove computers from the domain. Use an account that is, or leave on this computer only.".to_owned());
    }
    None
}

fn realm_leave(
    ctx: &JobContext<'_>,
    r: &Reporter<'_>,
    domain: &str,
    remove_as: Option<(&str, &str)>,
) -> Result<crate::cmd::CommandOutput, StepError> {
    let mut args = vec!["leave", "--unattended", "--verbose"];
    let user_arg;
    let mut spec_stdin = None;
    if let Some((user, password)) = remove_as {
        user_arg = format!("--user={user}");
        args.push("--remove");
        args.push(&user_arg);
        let mut secret = Zeroizing::new(String::with_capacity(password.len() + 1));
        secret.push_str(password);
        secret.push('\n');
        spec_stdin = Some(secret);
    }
    args.push(domain);
    let mut spec = CommandSpec::new("realm", &args).timeout(Duration::from_secs(5 * 60));
    if let Some(s) = spec_stdin {
        spec = spec.stdin(s);
    }
    let out = ctx
        .runner
        .run(&spec)
        .map_err(|e| StepError(format!("Could not run realm: {e}")))?;
    r.log_command(&spec.display(), &out);
    Ok(out)
}

#[allow(clippy::too_many_lines)] // one step after another; splitting would hide the order
fn leave_inner(
    ctx: &JobContext<'_>,
    req: &LeaveRequest,
    password: Zeroizing<String>,
    r: &Reporter<'_>,
) -> Result<Outcome, StepError> {
    let store = Store::new(ctx.paths.clone());
    let mut state = store
        .load()
        .map_err(|e| StepError(format!("Could not read Rabita's state: {e}")))?;
    let realm_entry = system::joined_realm(ctx.runner);
    let mut outcome = Outcome {
        domain: state.domain.clone().or_else(|| {
            realm_entry
                .as_ref()
                .and_then(|e| e.domain_name.clone().or(Some(e.name.clone())))
        }),
        computer_name: state.computer_name.clone(),
        ..Outcome::default()
    };

    // 1. realm leave (and remove the computer account)
    let (left, locally_only) = step(r, 0, |r| {
        let Some(domain) = outcome.domain.clone() else {
            return Ok((
                StepState::Skipped,
                "Not joined to a domain.".to_owned(),
                (false, false),
            ));
        };
        if realm_entry.is_none() {
            return Ok((
                StepState::Skipped,
                "This computer is not a member of a domain.".to_owned(),
                (false, false),
            ));
        }
        if !req.remove_computer_account {
            let out = realm_leave(ctx, r, &domain, None)?;
            if !out.success() {
                return Err(StepError(format!(
                    "Leaving the domain failed: {}",
                    out.combined().lines().last().unwrap_or("unknown error")
                )));
            }
            return Ok((
                StepState::Done,
                format!("Left {domain} on this computer only."),
                (true, true),
            ));
        }
        let user = req
            .admin_user
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .ok_or("Enter the IT admin account that can remove this computer, or choose to leave on this computer only.")?;
        let out = realm_leave(ctx, r, &domain, Some((user, &password)))?;
        if out.success() {
            return Ok((
                StepState::Done,
                format!("Left {domain} and removed this computer's account."),
                (true, false),
            ));
        }
        let text = out.combined();
        if let Some(problem) = credential_problem(&text) {
            return Err(StepError(problem));
        }
        r.log(format!(
            "Warning: could not reach {domain} to remove the computer account ({}). Leaving on this computer only.",
            text.lines().last().unwrap_or("no details")
        ));
        let local = realm_leave(ctx, r, &domain, None)?;
        if !local.success() {
            return Err(StepError(format!(
                "Leaving the domain failed: {}",
                local.combined().lines().last().unwrap_or("unknown error")
            )));
        }
        Ok((
            StepState::Done,
            format!("Left {domain} on this computer only: the domain could not be reached to remove the computer account."),
            (true, true),
        ))
    })?;
    drop(password);
    outcome.left = left;
    outcome.left_locally_only = locally_only;
    if left {
        state.joined = false;
        state.setup_finished = None;
        state.remaining_steps.clear();
        store
            .save(&state)
            .map_err(|e| StepError(format!("Could not save Rabita's state: {e}")))?;
    }

    // From here on, put back as much as possible; note what fails.
    let mut problems: Vec<String> = Vec::new();

    // 2. restore files
    if let Err(e) = step(r, 1, |r| {
        let notes = store
            .restore_all(&state)
            .map_err(|e| StepError(format!("Restoring files failed: {e}")))?;
        for n in &notes {
            r.log(n.clone());
        }
        if notes.is_empty() {
            Ok((StepState::Skipped, "No files to restore.".to_owned(), ()))
        } else {
            outcome.restored = true;
            Ok((
                StepState::Done,
                format!("Restored {} file(s).", notes.len()),
                (),
            ))
        }
    }) {
        problems.push(e.0);
    }

    let info = system::detect(ctx.runner, ctx.paths);
    let backend = info.family.unwrap_or(DistroFamily::Debian).backend();
    let facts = SystemFacts {
        mkhomedir_already_enabled: state.mkhomedir_was_enabled,
        ..system::gather_facts(ctx.paths)
    };

    // 3. login integration
    if let Err(e) = step(r, 2, |r| {
        if !left && state.backups.is_empty() {
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
    }) {
        problems.push(e.0);
    }

    // 4. display manager
    if let Err(e) = step(r, 3, |r| {
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
    }) {
        problems.push(e.0);
    }

    // 5. DNS server
    if let Err(e) = step(r, 4, |_| {
        if state.dns_change.is_none() {
            return Ok((
                StepState::Skipped,
                "Rabita did not change the DNS server.".to_owned(),
                (),
            ));
        }
        let result = netconf::restore_dns_inner(ctx.runner, &store, &mut state);
        if result.ok {
            outcome.restored = true;
            Ok((StepState::Done, result.message, ()))
        } else {
            Err(StepError(result.message))
        }
    }) {
        problems.push(e.0);
    }

    // 6. computer name (its files came back in step 2; make it live)
    if let Err(e) = step(r, 5, |r| {
        let Some(original) = state.original_hostname.clone() else {
            return Ok((
                StepState::Skipped,
                "Rabita did not rename this computer.".to_owned(),
                (),
            ));
        };
        execute_actions(
            ctx,
            &store,
            &mut state,
            r,
            &[Action::run(
                "Put back the computer name",
                "hostnamectl",
                &["set-hostname", &original],
            )],
            Duration::from_secs(60),
        )?;
        state.original_hostname = None;
        outcome.restored = true;
        Ok((
            StepState::Done,
            format!("This computer is called “{original}” again."),
            (),
        ))
    }) {
        problems.push(e.0);
    }

    // 7. clean up
    let finish = step(r, 6, |r| {
        if ctx.runner.which("systemctl") {
            if let Ok(out) = ctx
                .runner
                .run(&CommandSpec::new("systemctl", &["try-restart", "sssd"]))
            {
                r.log_command("systemctl try-restart sssd", &out);
            }
        }
        if problems.is_empty() {
            store
                .clear()
                .map_err(|e| StepError(format!("Could not clear Rabita's state: {e}")))?;
            Ok((StepState::Done, "Done.".to_owned(), ()))
        } else {
            // Keep the records so the next run can finish putting things back.
            store
                .save(&state)
                .map_err(|e| StepError(format!("Could not save Rabita's state: {e}")))?;
            Ok((
                StepState::Done,
                "Kept Rabita's records so the remaining settings can be put back later.".to_owned(),
                (),
            ))
        }
    });
    if let Err(e) = finish {
        problems.push(e.0);
    }
    outcome.problems = problems;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::backup::State;
    use crate::cmd::FakeRunner;
    use crate::paths::Paths;
    use crate::types::DnsChange;

    const MEMBER: &str =
        "lab.quietbyte.test\n  domain-name: lab.quietbyte.test\n  configured: kerberos-member\n";

    fn joined_lab(dir: &Path) -> (Paths, Store) {
        let paths = Paths::under(dir);
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
        write("/etc/krb5.conf", "[libdefaults]\n# original\n");
        write("/etc/hostname", "lab-classroom-pc-01\n");
        write("/etc/hosts", "127.0.1.1\tlab-classroom-pc-01\n");
        let mut state = State {
            domain: Some("lab.quietbyte.test".into()),
            computer_name: Some("LAB-CLASSROO-01".into()),
            joined: true,
            ..State::default()
        };
        // What a join did: changed krb5.conf, created sssd.conf and a
        // LightDM drop-in, renamed the computer, set the DNS server.
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
        store
            .write_file(
                &mut state,
                Path::new("/etc/hostname"),
                "lab-classroo-01\n",
                0o644,
            )
            .unwrap();
        store
            .write_file(
                &mut state,
                Path::new("/etc/hosts"),
                "127.0.1.1\tlab-classroo-01\n",
                0o644,
            )
            .unwrap();
        state.original_hostname = Some("lab-classroom-pc-01".into());
        state.dns_change = Some(DnsChange {
            connection_uuid: "6f2a-11".into(),
            connection_name: "Wired connection 1".into(),
            device: "enp1s0".into(),
            server: "10.0.0.10".into(),
            domain: "lab.quietbyte.test".into(),
            original_ignore_auto_dns: "no".into(),
            original_ipv6_ignore_auto_dns: "no".into(),
            ..DnsChange::default()
        });
        store.save(&state).unwrap();
        (paths, store)
    }

    fn ctx<'a>(fake: &'a FakeRunner, paths: &'a Paths, sink: &'a Collector) -> JobContext<'a> {
        JobContext {
            runner: fake,
            paths,
            sink,
            network: None,
        }
    }

    fn read(paths: &Paths, abs: &str) -> Option<String> {
        fs::read_to_string(paths.resolve(Path::new(abs))).ok()
    }

    fn removing() -> LeaveRequest {
        LeaveRequest {
            remove_computer_account: true,
            admin_user: Some("joiner".into()),
        }
    }

    #[test]
    fn leave_removes_the_account_and_puts_everything_back() {
        let dir = tempfile::tempdir().unwrap();
        let (paths, store) = joined_lab(dir.path());
        let fake = FakeRunner::new();
        fake.ok("realm list", MEMBER).ok("realm leave", "");
        let sink = Collector::new();
        let result = run_leave(
            &ctx(&fake, &paths, &sink),
            &removing(),
            Zeroizing::new("pw".into()),
        );
        assert!(result.ok, "{}", result.summary);
        assert!(!result.left_locally_only);
        assert_eq!(
            result.summary,
            "This computer has left lab.quietbyte.test, its computer account was removed, and its original settings are restored."
        );
        let calls = fake.calls();
        let leave = calls
            .iter()
            .find(|c| c.args.first().is_some_and(|a| a == "leave"))
            .unwrap();
        assert_eq!(
            leave.args,
            [
                "leave",
                "--unattended",
                "--verbose",
                "--remove",
                "--user=joiner",
                "lab.quietbyte.test"
            ]
        );
        assert_eq!(leave.stdin.as_deref().map(String::as_str), Some("pw\n"));

        // Files: Kerberos, sssd, login screen, hostname and hosts.
        assert_eq!(
            read(&paths, "/etc/krb5.conf").as_deref(),
            Some("[libdefaults]\n# original\n")
        );
        assert_eq!(read(&paths, "/etc/sssd/sssd.conf"), None);
        assert_eq!(
            read(&paths, "/etc/lightdm/lightdm.conf.d/90-rabita.conf"),
            None
        );
        assert_eq!(
            read(&paths, "/etc/hostname").as_deref(),
            Some("lab-classroom-pc-01\n")
        );
        assert_eq!(
            read(&paths, "/etc/hosts").as_deref(),
            Some("127.0.1.1\tlab-classroom-pc-01\n")
        );
        // PAM, DNS and the live host name.
        let lines = fake.call_lines();
        assert!(lines
            .iter()
            .any(|l| l.starts_with("pam-auth-update --package --remove")));
        assert!(lines.contains(
            &"nmcli connection modify 6f2a-11 ipv4.dns  ipv4.dns-search  ipv4.ignore-auto-dns no ipv6.ignore-auto-dns no".to_owned()
        ));
        assert!(lines.contains(&"hostnamectl set-hostname lab-classroom-pc-01".to_owned()));
        let ids: Vec<(&str, StepState)> = result
            .steps
            .iter()
            .map(|s| (s.id.as_str(), s.state))
            .collect();
        assert_eq!(
            ids,
            [
                ("leave", StepState::Done),
                ("restore", StepState::Done),
                ("login", StepState::Done),
                ("display", StepState::Skipped),
                ("dns", StepState::Done),
                ("name", StepState::Done),
                ("finish", StepState::Done),
            ]
        );
        assert!(!store.exists(), "records cleared");
    }

    /// GUI requirement: if the directory can't be reached, leave locally and
    /// say the computer account must be deleted in AD.
    #[test]
    fn an_unreachable_directory_falls_back_to_leaving_locally() {
        let dir = tempfile::tempdir().unwrap();
        let (paths, _) = joined_lab(dir.path());
        let fake = FakeRunner::new();
        fake.ok("realm list", MEMBER)
            .fail(
                "realm leave",
                "realm: Couldn't connect to lab.quietbyte.test: Cannot contact any KDC",
            )
            .ok("realm leave", "");
        let sink = Collector::new();
        let result = run_leave(
            &ctx(&fake, &paths, &sink),
            &removing(),
            Zeroizing::new("pw".into()),
        );
        assert!(result.ok, "{}", result.summary);
        assert!(result.left_locally_only);
        assert_eq!(
            result.summary,
            "This computer has left lab.quietbyte.test and its original settings are restored. Its computer account (LAB-CLASSROO-01) is still in the directory: ask your IT admin to delete it in Active Directory Users and Computers."
        );
        let leaves: Vec<Vec<String>> = fake
            .calls()
            .into_iter()
            .filter(|c| c.args.first().is_some_and(|a| a == "leave"))
            .map(|c| c.args)
            .collect();
        assert_eq!(leaves.len(), 2);
        assert!(leaves[0].contains(&"--remove".to_owned()));
        assert!(
            !leaves[1].contains(&"--remove".to_owned()),
            "second try is local only"
        );
        let leave = result.steps.iter().find(|s| s.id == "leave").unwrap();
        assert!(leave.message.contains("on this computer only"));
        assert!(sink.events().iter().any(
            |e| matches!(e, crate::progress::ProgressEvent::Log(l) if l.contains("could not reach"))
        ));
    }

    #[test]
    fn a_wrong_password_stops_before_anything_changes() {
        let dir = tempfile::tempdir().unwrap();
        let (paths, store) = joined_lab(dir.path());
        let fake = FakeRunner::new();
        fake.ok("realm list", MEMBER).fail(
            "realm leave",
            "realm: Couldn't authenticate as joiner: Preauthentication failed",
        );
        let sink = Collector::new();
        let result = run_leave(
            &ctx(&fake, &paths, &sink),
            &removing(),
            Zeroizing::new("bad".into()),
        );
        assert!(!result.ok);
        assert_eq!(
            result.summary,
            "The administrator username or password was not accepted."
        );
        assert_eq!(
            fake.calls()
                .iter()
                .filter(|c| c.args.first().is_some_and(|a| a == "leave"))
                .count(),
            1
        );
        assert!(
            store.load().unwrap().joined,
            "still joined, nothing restored"
        );
        assert_eq!(
            read(&paths, "/etc/krb5.conf").as_deref(),
            Some("[libdefaults]\n# changed\n")
        );
        assert!(
            credential_problem("Insufficient permissions to remove the computer account")
                .unwrap()
                .contains("not allowed")
        );
    }

    #[test]
    fn leaving_without_an_admin_account_is_local_only() {
        let dir = tempfile::tempdir().unwrap();
        let (paths, _) = joined_lab(dir.path());
        let fake = FakeRunner::new();
        fake.ok("realm list", MEMBER);
        let sink = Collector::new();
        let result = run_leave(
            &ctx(&fake, &paths, &sink),
            &LeaveRequest::default(),
            Zeroizing::new(String::new()),
        );
        assert!(result.ok && result.left_locally_only);
        assert!(
            fake.calls().iter().all(|c| c.stdin.is_none()),
            "no password involved"
        );
        assert!(fake
            .call_lines()
            .contains(&"realm leave --unattended --verbose lab.quietbyte.test".to_owned()));
    }

    #[test]
    fn something_not_put_back_keeps_the_records_for_next_time() {
        let dir = tempfile::tempdir().unwrap();
        let (paths, store) = joined_lab(dir.path());
        let fake = FakeRunner::new();
        fake.ok("realm list", MEMBER)
            .fail("nmcli connection", "Error: NetworkManager is not running.");
        let sink = Collector::new();
        let result = run_leave(
            &ctx(&fake, &paths, &sink),
            &removing(),
            Zeroizing::new("pw".into()),
        );
        assert!(!result.ok);
        assert!(result.summary.starts_with(
            "This computer has left lab.quietbyte.test, but some settings could not be put back: Could not put the original DNS setting back"
        ), "{}", result.summary);
        let dns = result.steps.iter().find(|s| s.id == "dns").unwrap();
        assert_eq!(dns.state, StepState::Failed);
        let name = result.steps.iter().find(|s| s.id == "name").unwrap();
        assert_eq!(name.state, StepState::Done, "later steps still ran");
        let state = store.load().unwrap();
        assert!(!state.joined);
        assert!(state.dns_change.is_some() && state.restore_pending());
    }

    /// The same job undoes a join that never completed.
    #[test]
    fn undoes_changes_from_a_join_that_never_completed() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let p = paths.os_release();
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "ID=ubuntu\n").unwrap();
        let store = Store::new(paths.clone());
        store
            .save(&State {
                domain: Some("lab.test".into()),
                dns_change: Some(DnsChange {
                    connection_uuid: "u".into(),
                    connection_name: "Wired connection 1".into(),
                    device: "enp1s0".into(),
                    original_ignore_auto_dns: "no".into(),
                    original_ipv6_ignore_auto_dns: "no".into(),
                    ..DnsChange::default()
                }),
                ..State::default()
            })
            .unwrap();
        let fake = FakeRunner::new();
        fake.ok("realm list", "");
        let sink = Collector::new();
        let result = run_leave(
            &ctx(&fake, &paths, &sink),
            &LeaveRequest::default(),
            Zeroizing::new(String::new()),
        );
        assert!(result.ok, "{}", result.summary);
        assert_eq!(
            result.summary,
            "Rabita put back the settings it had changed."
        );
        assert!(!result.left_locally_only);
        assert!(!fake
            .call_lines()
            .iter()
            .any(|l| l.starts_with("realm leave")));
        assert!(fake
            .call_lines()
            .iter()
            .any(|l| l.starts_with("nmcli connection modify u")));
        assert!(!store.exists());
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
        let sink = Collector::new();
        let result = run_leave(
            &ctx(&fake, &paths, &sink),
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
