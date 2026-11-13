// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! The `Join` job: connect this computer to the organization.
//!
//! Steps (each reported live, each reversible through the backup store):
//! 1. detect   — is this computer supported?
//! 2. packages — install realmd, sssd, adcli and friends
//! 3. time     — find the domain controllers and check the clock
//! 4. join     — `realm join` through the recommended domain controller,
//!    with the admin password on stdin
//! 5. sssd     — plain usernames, offline login, `/home/<user>`, preferred
//!    DC; checked with `sssctl config-check` before restarting
//! 6. access   — who may sign in (everyone, or only listed groups)
//! 7. home     — create home folders on first login
//! 8. display  — username box on the login screen
//! 9. verify   — Kerberos ticket from the machine keytab, user lookup
//!
//! Steps 5-9 run after the computer account exists. If one fails, the
//! computer is "joined but not finished": the state file lists the steps
//! still to run and `ApplyOptions` (Finish setup) completes them.

use std::path::Path;
use std::time::{Duration, Instant};

use rabita_backends::{Action, Backend, DisplayManager, SystemFacts};
use zeroize::Zeroizing;

use crate::backup::{State, Store};
use crate::cmd::{CommandOutput, CommandSpec, Runner};
use crate::discover::{self, Network};
use crate::ini::IniDocument;
use crate::paths::Paths;
use crate::progress::{ProgressSink, Reporter};
use crate::system;
use crate::types::{JobResult, JoinOptions, JoinRequest, LoginAccess, StepOutcome, StepState};

/// Everything a job needs from its surroundings.
pub struct JobContext<'a> {
    /// Runs system tools.
    pub runner: &'a dyn Runner,
    /// Where files live.
    pub paths: &'a Paths,
    /// Receives progress.
    pub sink: &'a dyn ProgressSink,
    /// The network to probe (DNS, TCP, SNTP). `None` skips network checks;
    /// tests pass a fake that replays a real domain's layout.
    pub network: Option<&'a dyn Network>,
}

/// Maximum clock difference (seconds) we accept without fixing time sync.
const CLOCK_OK_SECS: f64 = 120.0;
/// Kerberos' own limit; beyond this a join cannot work.
const CLOCK_HARD_LIMIT_SECS: f64 = 300.0;

/// A step failed; the message is for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepError(pub String);

impl From<String> for StepError {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for StepError {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

/// Runs one planned [`Action`], logging it and recording backups.
pub fn execute_action(
    ctx: &JobContext<'_>,
    store: &Store,
    state: &mut State,
    reporter: &Reporter<'_>,
    action: &Action,
    timeout: Duration,
) -> Result<(), StepError> {
    match action {
        Action::Run {
            description,
            program,
            args,
            env,
            stdin,
            required,
        } => {
            reporter.log(format!("{description}…"));
            let mut spec = CommandSpec::new(program, &[]).timeout(timeout);
            spec.args.clone_from(args);
            spec.env.clone_from(env);
            if let Some(data) = stdin {
                spec = spec.stdin(Zeroizing::new(data.clone()));
            }
            let display = spec.display();
            match ctx.runner.run(&spec) {
                Ok(out) => {
                    reporter.log_command(&display, &out);
                    if out.success() || !required {
                        Ok(())
                    } else {
                        Err(StepError(format!(
                            "{description} failed: {}",
                            last_lines(&out.combined(), 3)
                        )))
                    }
                }
                Err(e) if *required => Err(StepError(format!("{description} failed: {e}"))),
                Err(e) => {
                    reporter.log(format!("  skipped: {e}"));
                    Ok(())
                }
            }
        }
        Action::WriteFile {
            description,
            path,
            content,
            mode,
        } => {
            reporter.log(format!("{description}: writing {}", path.display()));
            store
                .write_file(state, path, content, *mode)
                .map_err(|e| StepError(format!("{description} failed: {e}")))
        }
        Action::RemoveFile { description, path } => {
            reporter.log(format!("{description}: removing {}", path.display()));
            match std::fs::remove_file(ctx.paths.resolve(path)) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(StepError(format!("{description} failed: {e}"))),
            }
        }
    }
}

/// Runs a list of actions; the first failure stops the list.
pub fn execute_actions(
    ctx: &JobContext<'_>,
    store: &Store,
    state: &mut State,
    reporter: &Reporter<'_>,
    actions: &[Action],
    timeout: Duration,
) -> Result<(), StepError> {
    for a in actions {
        execute_action(ctx, store, state, reporter, a, timeout)?;
    }
    Ok(())
}

fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join(" / ")
}

/// Turns `realm join` output into a sentence a non-expert can act on.
#[must_use]
pub fn explain_realm_join_failure(output: &str) -> String {
    let lower = output.to_ascii_lowercase();
    if lower.contains("already joined") || lower.contains("already a member") {
        "This computer is already joined to a domain. Leave it first, then try again.".to_owned()
    } else if lower.contains("couldn't authenticate")
        || lower.contains("preauthentication failed")
        || lower.contains("password incorrect")
        || lower.contains("client not found in kerberos database")
    {
        "The administrator username or password was not accepted.".to_owned()
    } else if lower.contains("insufficient permissions")
        || lower.contains("permission denied")
        || lower.contains("not allowed to join")
        || lower.contains("access denied")
    {
        "That account is not allowed to join computers to the domain. Ask your IT admin to delegate the 'join computers' right, or use a different account.".to_owned()
    } else if lower.contains("no such realm")
        || lower.contains("cannot resolve")
        || lower.contains("couldn't find")
        || lower.contains("not discovered")
    {
        "The domain could not be found. Check the domain name and that this computer uses the organization's DNS server.".to_owned()
    } else if lower.contains("clock skew") {
        "This computer's clock is too far from the domain controller's. Fix the time and try again."
            .to_owned()
    } else if lower.contains("organizational unit")
        || lower.contains("ou=") && lower.contains("not found")
    {
        "The organizational unit (OU) could not be found. Check the OU path with your IT admin."
            .to_owned()
    } else {
        format!("Joining failed: {}", last_lines(output, 3))
    }
}

/// Edits `sssd.conf` the way the wizard promises: plain usernames, offline
/// login, `/home/<user>`, and the healthiest domain controller first.
///
/// `default_domain_suffix` is removed wherever it appears: SSSD 2.10 and
/// newer refuse to start when it is combined with
/// `use_fully_qualified_names = False`, and plain usernames work without it
/// because there is only one domain. `config_file_version` is removed too;
/// SSSD 2 no longer uses it.
#[must_use]
pub fn configure_sssd_text(
    text: &str,
    domain: &str,
    offline_login: bool,
    preferred_dc: Option<&str>,
) -> String {
    let mut doc = IniDocument::parse(text);
    doc.remove_everywhere("default_domain_suffix");
    doc.remove("sssd", "config_file_version");
    let changed = doc.set_in_sections_with_prefix("domain/", "use_fully_qualified_names", "False");
    if changed == 0 {
        // No domain section yet (should not happen after realm join); create one.
        doc.set(
            &format!("domain/{domain}"),
            "use_fully_qualified_names",
            "False",
        );
    }
    doc.set_in_sections_with_prefix(
        "domain/",
        "cache_credentials",
        if offline_login { "True" } else { "False" },
    );
    doc.set_in_sections_with_prefix("domain/", "fallback_homedir", "/home/%u");
    // Let AD users log in even when no GPO grants "Allow log on locally"
    // to Linux machines; most schools never set one for Linux.
    doc.set_in_sections_with_prefix("domain/", "ad_gpo_access_control", "permissive");
    if let Some(dc) = preferred_dc.map(str::trim).filter(|d| !d.is_empty()) {
        // Talk to the healthiest DC first; `_srv_` falls back to DNS
        // discovery if it is unavailable.
        doc.set_in_sections_with_prefix("domain/", "ad_server", &format!("{dc}, _srv_"));
    }
    doc.render()
}

/// Whether `sssd.conf` has a setting that stops SSSD 2.10+ from starting
/// (written by Rabita before this was known).
#[must_use]
pub fn sssd_conf_needs_repair(text: &str) -> bool {
    IniDocument::parse(text).has_key_anywhere("default_domain_suffix")
}

/// Problems reported by `sssctl config-check`, one line each. Empty when
/// the configuration is valid.
#[must_use]
pub fn sssctl_problems(out: &CommandOutput) -> Vec<String> {
    let text = out.combined();
    let mut problems: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("[rule/") || l.to_ascii_lowercase().contains("check failed"))
        .map(str::to_owned)
        .collect();
    let issue_count = text.lines().find_map(|l| {
        l.trim()
            .strip_prefix("Issues identified by validators:")
            .and_then(|n| n.trim().parse::<usize>().ok())
    });
    if problems.is_empty() && (!out.success() || issue_count.is_some_and(|n| n > 0)) {
        let tail = last_lines(&text, 3);
        problems.push(if tail.is_empty() {
            "sssctl config-check failed without saying why".to_owned()
        } else {
            tail
        });
    }
    problems
}

/// The domain controller adcli says it used. `realm join --verbose` passes
/// adcli's ` * Using domain controller: <host>` line through.
#[must_use]
pub fn used_domain_controller(output: &str) -> Option<String> {
    output.lines().find_map(|l| {
        l.trim()
            .trim_start_matches('*')
            .trim()
            .strip_prefix("Using domain controller:")
            .map(|h| h.trim().to_owned())
            .filter(|h| !h.is_empty())
    })
}

/// Whether `realm join` failed while discovering the name it was given,
/// before talking to the domain at all.
fn is_discovery_failure(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    lower.contains("no such realm")
        || lower.contains("couldn't find")
        || lower.contains("couldn't discover")
        || lower.contains("not discovered")
        || lower.contains("cannot resolve")
}

/// Actions that set who may sign in, via `realm permit` / `realm deny`.
///
/// For a group list the previous permissions are cleared first
/// (`realm deny --all`) so the list is exact, not additive.
pub fn login_access_actions(access: &LoginAccess) -> Result<Vec<Action>, StepError> {
    match access {
        LoginAccess::All => Ok(vec![Action::run(
            "Allow all domain users to sign in",
            "realm",
            &["permit", "--all"],
        )]),
        LoginAccess::Groups { groups } => {
            let groups: Vec<&str> = groups
                .iter()
                .map(|g| g.trim())
                .filter(|g| !g.is_empty())
                .collect();
            if groups.is_empty() {
                return Err(StepError::from(
                    "Enter at least one group, or choose 'All domain users'.",
                ));
            }
            let mut args = vec!["permit", "-g"];
            args.extend(groups.iter().copied());
            Ok(vec![
                Action::run(
                    "Clear previous sign-in permissions",
                    "realm",
                    &["deny", "--all"],
                ),
                Action::run(
                    &format!("Allow only these groups to sign in: {}", groups.join(", ")),
                    "realm",
                    &args,
                ),
            ])
        }
    }
}

/// Reads the first `host/` or machine-account principal from `klist -k`.
#[must_use]
pub fn keytab_principal(klist_output: &str) -> Option<String> {
    let mut fallback = None;
    for line in klist_output.lines() {
        let mut parts = line.split_whitespace();
        let Some(first) = parts.next() else { continue };
        if first.parse::<u32>().is_err() {
            continue;
        }
        let Some(principal) = parts.next() else {
            continue;
        };
        if !principal.contains('@') {
            continue;
        }
        if principal
            .split('@')
            .next()
            .is_some_and(|p| p.ends_with('$'))
        {
            return Some(principal.to_owned());
        }
        if fallback.is_none() {
            fallback = Some(principal.to_owned());
        }
    }
    fallback
}

/// Runs steps one at a time and reports each one's progress.
pub(crate) struct Steps<'a> {
    reporter: Reporter<'a>,
}

impl<'a> Steps<'a> {
    /// Wraps a reporter.
    pub(crate) fn new(reporter: Reporter<'a>) -> Self {
        Self { reporter }
    }

    pub(crate) fn run<T>(
        &self,
        id: &str,
        label: &str,
        f: impl FnOnce(&Reporter<'_>) -> Result<(StepState, String, T), StepError>,
    ) -> Result<T, StepError> {
        self.reporter.step(id, label, StepState::Running, "");
        match f(&self.reporter) {
            Ok((state, msg, value)) => {
                self.reporter.step(id, label, state, &msg);
                Ok(value)
            }
            Err(e) => {
                self.reporter.step(id, label, StepState::Failed, &e.0);
                Err(e)
            }
        }
    }
}

const JOIN_STEPS: [(&str, &str); 9] = [
    ("detect", "Check this computer"),
    ("packages", "Install domain login software"),
    ("time", "Find the domain and check the clock"),
    ("join", "Join the domain"),
    ("sssd", "Set up sign-in"),
    ("access", "Choose who can sign in"),
    ("home", "Create home folders on first login"),
    ("display", "Show a username box on the login screen"),
    ("verify", "Check that it works"),
];

/// Ids of the steps that run after the computer account exists, in order.
/// If one of them fails, the computer is "joined but not finished" and
/// `ApplyOptions` can run them again to finish setup.
pub const POST_JOIN_STEPS: [&str; 5] = ["sssd", "access", "home", "display", "verify"];

/// Plain-language label for a step id.
#[must_use]
pub fn step_label(id: &str) -> &'static str {
    JOIN_STEPS
        .iter()
        .find(|(i, _)| *i == id)
        .map_or("Unknown step", |(_, l)| *l)
}

/// The summary for a computer that is in the domain but not fully set up.
#[must_use]
pub fn not_finished_summary(domain: &str, problem: &str, remaining: &[String]) -> String {
    let labels: Vec<&str> = remaining.iter().map(|id| step_label(id)).collect();
    format!(
        "This computer is joined to {domain} but not finished. {}. Until setup is finished, domain sign-in may not work properly (for example, home folders may not be created). Fix the problem, then run Finish setup (rabitactl finish) to complete the remaining steps: {}.",
        problem.trim().trim_end_matches('.'),
        labels.join(", ")
    )
}

/// Marks steps that never started after a failure, so the result says why.
pub(crate) fn mark_not_run(steps: &mut [StepOutcome], joined_not_finished: bool) {
    for s in steps {
        if s.state == StepState::Pending {
            s.state = StepState::Skipped;
            if joined_not_finished {
                "Not run yet. Finish setup will run it.".clone_into(&mut s.message);
            } else {
                "Not run because an earlier step failed.".clone_into(&mut s.message);
            }
        }
    }
}

/// What the steps after joining need to know.
pub struct PostJoin<'a> {
    /// Domain name.
    pub domain: &'a str,
    /// Options to apply (login access already normalised).
    pub options: &'a JoinOptions,
    /// Domain user looked up by the verify step, if any.
    pub verify_user: Option<&'a str>,
    /// Healthiest domain controller, preferred in `sssd.conf`.
    pub preferred_dc: Option<&'a str>,
    /// Which of [`POST_JOIN_STEPS`] to run, in order.
    pub steps: &'a [&'a str],
    /// When an option is off, undo it (`ApplyOptions`) instead of skipping
    /// it (`Join`, where there is nothing to undo).
    pub undo_disabled: bool,
    /// Record progress so a failure leaves the computer marked "joined but
    /// not finished" with the steps still to run.
    pub track_unfinished: bool,
}

type StepResult = Result<(StepState, String, ()), StepError>;

/// Runs the post-join steps in `pj.steps` and stops at the first failure.
///
/// With `track_unfinished`, the state file records which steps are still to
/// run after every step, so a failure (or a crash) leaves the computer marked
/// "joined but not finished", and success marks setup finished.
pub(crate) fn run_post_join(
    ctx: &JobContext<'_>,
    store: &Store,
    state: &mut State,
    backend: &dyn Backend,
    display_manager: DisplayManager,
    pj: &PostJoin<'_>,
    steps: &Steps<'_>,
) -> Result<(), StepError> {
    let save = |state: &State| {
        store
            .save(state)
            .map_err(|e| StepError(format!("Could not save Rabita's state: {e}")))
    };
    let remaining_from = |i: usize| -> Vec<String> {
        pj.steps[i.min(pj.steps.len())..]
            .iter()
            .map(|s| (*s).to_owned())
            .collect()
    };
    let access_actions = login_access_actions(&pj.options.login_access)?;
    if pj.track_unfinished {
        state.setup_finished = Some(false);
        state.remaining_steps = remaining_from(0);
        save(state)?;
    }
    let facts = SystemFacts {
        mkhomedir_already_enabled: state.mkhomedir_was_enabled,
        ..system::gather_facts(ctx.paths)
    };
    for (i, id) in pj.steps.iter().enumerate() {
        let outcome = steps.run(id, step_label(id), |r| match *id {
            "sssd" => sssd_step(ctx, store, state, r, pj),
            "access" => {
                execute_actions(
                    ctx,
                    store,
                    state,
                    r,
                    &access_actions,
                    Duration::from_secs(120),
                )?;
                Ok((StepState::Done, pj.options.login_access.describe(), ()))
            }
            "home" => home_step(ctx, store, state, r, backend, &facts, pj),
            "display" => display_step(ctx, store, state, r, backend, &facts, display_manager, pj),
            "verify" => verify_step(ctx, r, pj),
            other => Err(StepError(format!("Unknown step '{other}'."))),
        });
        if pj.track_unfinished {
            state.remaining_steps = remaining_from(if outcome.is_ok() { i + 1 } else { i });
            if outcome.is_err() {
                // Best effort: the step's own error is the one to report.
                let _ = save(state);
            } else {
                save(state)?;
            }
        }
        outcome?;
    }
    if pj.track_unfinished {
        state.setup_finished = Some(true);
        state.remaining_steps.clear();
    }
    save(state)
}

fn sssd_step(
    ctx: &JobContext<'_>,
    store: &Store,
    state: &mut State,
    r: &Reporter<'_>,
    pj: &PostJoin<'_>,
) -> StepResult {
    let text = std::fs::read_to_string(ctx.paths.sssd_conf()).unwrap_or_default();
    let new_text = configure_sssd_text(&text, pj.domain, pj.options.offline_login, pj.preferred_dc);
    store
        .write_file(state, Path::new("/etc/sssd/sssd.conf"), &new_text, 0o600)
        .map_err(|e| StepError(format!("Could not update sssd.conf: {e}")))?;
    r.log("Updated /etc/sssd/sssd.conf: plain usernames, offline login, /home/<user>");
    if let Some(dc) = pj.preferred_dc {
        r.log(format!(
            "Preferred domain controller: {dc} (others are used if it is unavailable)"
        ));
    }

    // Check the file before restarting, so a bad setting is reported in
    // plain words instead of as a service that will not start.
    let check = ctx
        .runner
        .run(&CommandSpec::new("sssctl", &["config-check"]).timeout(Duration::from_secs(60)))
        .map_err(|e| {
            StepError(format!(
                "Could not check the new sign-in settings ({e}). Install the sssd-tools package, then finish setup."
            ))
        })?;
    r.log_command("sssctl config-check", &check);
    let problems = sssctl_problems(&check);
    if !problems.is_empty() {
        let shown: Vec<&str> = problems.iter().take(3).map(String::as_str).collect();
        return Err(StepError(format!(
            "The new sign-in settings in /etc/sssd/sssd.conf have errors, so the sign-in service was not restarted: {}",
            shown.join("; ")
        )));
    }

    let actions = [
        Action::run(
            "Enable the sign-in service",
            "systemctl",
            &["enable", "sssd"],
        ),
        Action::run(
            "Restart the sign-in service",
            "systemctl",
            &["restart", "sssd"],
        ),
    ];
    if let Err(e) = execute_actions(ctx, store, state, r, &actions, Duration::from_secs(60)) {
        if let Ok(journal) = ctx.runner.run(&CommandSpec::new(
            "journalctl",
            &["-u", "sssd", "-n", "20", "--no-pager"],
        )) {
            r.log_command("journalctl -u sssd -n 20 --no-pager", &journal);
        }
        return Err(StepError(format!(
            "The sign-in service (SSSD) did not start, so domain sign-in will not work yet. {}. See 'journalctl -u sssd' for details",
            e.0.trim_end_matches('.')
        )));
    }
    Ok((
        StepState::Done,
        "Sign-in configured, checked and started.".to_owned(),
        (),
    ))
}

fn home_step(
    ctx: &JobContext<'_>,
    store: &Store,
    state: &mut State,
    r: &Reporter<'_>,
    backend: &dyn Backend,
    facts: &SystemFacts,
    pj: &PostJoin<'_>,
) -> StepResult {
    let (actions, done, nothing_to_do) = if pj.options.create_home {
        (
            backend.enable_login_integration(facts),
            "Home folders will be created on first login.",
            "Already enabled on this computer.",
        )
    } else if pj.undo_disabled {
        (
            backend.disable_login_integration(facts),
            "Home folders will no longer be created automatically.",
            "No change needed.",
        )
    } else {
        return Ok((StepState::Skipped, "Turned off in options.".to_owned(), ()));
    };
    if actions.is_empty() {
        return Ok((StepState::Skipped, nothing_to_do.to_owned(), ()));
    }
    execute_actions(ctx, store, state, r, &actions, Duration::from_secs(60))?;
    Ok((StepState::Done, done.to_owned(), ()))
}

#[allow(clippy::too_many_arguments)] // mirrors home_step plus the login screen in use
fn display_step(
    ctx: &JobContext<'_>,
    store: &Store,
    state: &mut State,
    r: &Reporter<'_>,
    backend: &dyn Backend,
    facts: &SystemFacts,
    display_manager: DisplayManager,
    pj: &PostJoin<'_>,
) -> StepResult {
    let (actions, done) = if pj.options.manual_login {
        (backend.enable_manual_login(facts), "Login screen updated.")
    } else if pj.undo_disabled {
        (
            backend.disable_manual_login(facts),
            "Username box removed from the login screen.",
        )
    } else {
        return Ok((StepState::Skipped, "Turned off in options.".to_owned(), ()));
    };
    if actions.is_empty() {
        return Ok((
            StepState::Skipped,
            format!("No change needed for this login screen ({display_manager:?})."),
            (),
        ));
    }
    execute_actions(ctx, store, state, r, &actions, Duration::from_secs(60))?;
    Ok((StepState::Done, done.to_owned(), ()))
}

fn verify_step(ctx: &JobContext<'_>, r: &Reporter<'_>, pj: &PostJoin<'_>) -> StepResult {
    let mut notes = Vec::new();
    let klist = ctx
        .runner
        .run(&CommandSpec::new("klist", &["-k", "/etc/krb5.keytab"]))
        .map_err(|e| StepError(format!("Could not read the computer's Kerberos keys: {e}")))?;
    r.log_command("klist -k /etc/krb5.keytab", &klist);
    let principal = keytab_principal(&klist.stdout).ok_or(
        "The computer's Kerberos key file has no entries. The join did not complete properly.",
    )?;
    let kinit = ctx
        .runner
        .run(&CommandSpec::new(
            "kinit",
            &[
                "-k",
                "-t",
                "/etc/krb5.keytab",
                "-c",
                "MEMORY:rabita-verify",
                &principal,
            ],
        ))
        .map_err(|e| StepError(format!("Could not run kinit: {e}")))?;
    r.log_command(
        &format!("kinit -k -t /etc/krb5.keytab -c MEMORY:rabita-verify {principal}"),
        &kinit,
    );
    if !kinit.success() {
        return Err(StepError(format!(
            "The computer could not get a Kerberos ticket: {}",
            last_lines(&kinit.combined(), 2)
        )));
    }
    notes.push("Kerberos ticket OK".to_owned());
    if let Some(user) = pj.verify_user.map(str::trim).filter(|u| !u.is_empty()) {
        let mut found = false;
        for _ in 0..5 {
            let getent = ctx
                .runner
                .run(&CommandSpec::new("getent", &["passwd", user]))
                .map_err(|e| StepError(format!("Could not run getent: {e}")))?;
            r.log_command(&format!("getent passwd {user}"), &getent);
            if getent.success() && !getent.stdout.trim().is_empty() {
                found = true;
                break;
            }
            std::thread::sleep(Duration::from_secs(if ctx.network.is_some() {
                2
            } else {
                0
            }));
        }
        if !found {
            return Err(StepError(format!(
                "The user '{user}' could not be looked up yet. Wait a minute and check Status; if it still fails, check the user name."
            )));
        }
        notes.push(format!("user '{user}' found"));
    }
    Ok((StepState::Done, notes.join("; "), ()))
}

/// Records that `realm join` added this computer to the domain.
fn mark_joined(state: &mut State, req: &JoinRequest, hostname: &str) {
    state.joined = true;
    state.joined_at = Some(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    );
    state.computer_name = Some(req.computer_name.clone().unwrap_or_else(|| {
        hostname
            .split('.')
            .next()
            .unwrap_or("")
            .to_ascii_uppercase()
    }));
}

/// Runs the whole join and returns the result. The password is wiped when
/// this function returns.
#[must_use]
pub fn run_join(ctx: &JobContext<'_>, req: &JoinRequest, password: Zeroizing<String>) -> JobResult {
    let collector = crate::progress::Collector::new();
    let tee = crate::progress::Tee(vec![ctx.sink, &collector]);
    let reporter = Reporter::new(&tee);
    for (id, label) in JOIN_STEPS {
        reporter.step(id, label, StepState::Pending, "");
    }
    let steps = Steps::new(Reporter::new(&tee));
    let mut joined_now = false;
    let outcome = join_inner(ctx, req, password, &steps, &mut joined_now);
    let state = Store::new(ctx.paths.clone()).load().unwrap_or_default();
    let unfinished =
        outcome.is_err() && joined_now && state.joined && state.setup_finished != Some(true);
    let mut steps_out = collector.outcomes();
    if outcome.is_err() {
        mark_not_run(&mut steps_out, unfinished);
    }
    let (domain, _) = discover::normalise_domain(&req.domain);
    JobResult {
        ok: outcome.is_ok(),
        summary: match &outcome {
            Ok(()) => format!("This computer is now connected to {domain}."),
            Err(e) if unfinished => not_finished_summary(&domain, &e.0, &state.remaining_steps),
            Err(e) => e.0.clone(),
        },
        steps: steps_out,
        joined_not_finished: unfinished,
        remaining_steps: if unfinished {
            state.remaining_steps
        } else {
            Vec::new()
        },
    }
}

#[allow(clippy::too_many_lines)] // one step after another; splitting would hide the order
fn join_inner(
    ctx: &JobContext<'_>,
    req: &JoinRequest,
    password: Zeroizing<String>,
    steps: &Steps<'_>,
    joined_now: &mut bool,
) -> Result<(), StepError> {
    let store = Store::new(ctx.paths.clone());
    let (domain, realm) = discover::normalise_domain(&req.domain);
    if !discover::looks_like_domain(&domain) {
        steps.run("detect", JOIN_STEPS[0].1, |_| {
            Err::<(StepState, String, ()), _>(StepError::from(
                "Enter the full domain name, for example school.example.org.",
            ))
        })?;
    }
    if req.admin_user.trim().is_empty() {
        steps.run("detect", JOIN_STEPS[0].1, |_| {
            Err::<(StepState, String, ()), _>(StepError::from(
                "Enter the administrator account name.",
            ))
        })?;
    }
    if let Err(e) = login_access_actions(&req.options.login_access) {
        steps.run("detect", JOIN_STEPS[0].1, |_| {
            Err::<(StepState, String, ()), _>(e)
        })?;
    }

    // 1. detect
    let (info, backend) = steps.run("detect", JOIN_STEPS[0].1, |r| {
        let info = system::detect(ctx.runner, ctx.paths);
        r.log(format!(
            "{} ({}), login screen: {:?}",
            info.distro_name, info.distro_id, info.display_manager
        ));
        if !info.supported {
            return Err(StepError(
                info.unsupported_reason.clone().unwrap_or_default(),
            ));
        }
        let prior = store.load().unwrap_or_default();
        if prior.joined && prior.setup_finished == Some(false) {
            return Err(StepError(format!(
                "This computer is already joined to {} but not finished. Run Finish setup (rabitactl finish) to complete it, or Leave domain to start again.",
                prior.domain.unwrap_or_default()
            )));
        }
        if let Some(d) = &info.joined_domain {
            return Err(StepError(format!(
                "This computer is already connected to {d}. Use 'Leave domain' first."
            )));
        }
        let family = info.family.ok_or("Unsupported system")?;
        let backend: Box<dyn Backend> = family.backend();
        Ok((
            StepState::Done,
            format!("{} is supported.", info.distro_name),
            (info, backend),
        ))
    })?;

    let mut options = req.options.clone();
    options.login_access = options.login_access.normalised();
    let mut state = State {
        domain: Some(domain.clone()),
        realm: Some(realm.clone()),
        options,
        verify_user: req.verify_user.clone(),
        ..State::default()
    };
    let facts_before = system::gather_facts(ctx.paths);
    state.mkhomedir_was_enabled = facts_before.mkhomedir_already_enabled;
    store
        .save(&state)
        .map_err(|e| StepError(format!("Could not save Rabita's state: {e}")))?;

    // 2. packages
    steps.run("packages", JOIN_STEPS[1].1, |r| {
        let needed: Vec<&str> = backend
            .packages()
            .into_iter()
            .filter(|p| !package_present(ctx.runner, backend.family(), p))
            .collect();
        if needed.is_empty() {
            return Ok((StepState::Skipped, "Already installed.".to_owned(), ()));
        }
        r.log(format!("Installing: {}", needed.join(" ")));
        let actions = backend.install_packages(&needed, &realm);
        execute_actions(
            ctx,
            &store,
            &mut state,
            r,
            &actions,
            Duration::from_secs(20 * 60),
        )?;
        for tool in ["realm", "adcli", "sssd", "sssctl"] {
            if !ctx.runner.which(tool) {
                return Err(StepError(format!(
                    "'{tool}' is still missing after installing packages."
                )));
            }
        }
        Ok((
            StepState::Done,
            format!("Installed {} package(s).", needed.len()),
            (),
        ))
    })?;

    // 3. time (and discovery)
    let dc = steps.run("time", JOIN_STEPS[2].1, |r| {
        let Some(net) = ctx.network else {
            return Ok((StepState::Skipped, "Network checks disabled.".to_owned(), None));
        };
        let disc = net.discover(&domain, ctx.runner, ctx.paths);
        if let Some(p) = &disc.problem {
            if disc.recommended.is_none() {
                return Err(StepError(p.clone()));
            }
        }
        for w in &disc.warnings {
            r.log(format!("Warning: {w}"));
        }
        let dc = disc
            .controllers
            .iter()
            .find(|c| c.recommended)
            .cloned()
            .ok_or("No domain controller could be reached.")?;
        if let Some(site) = &disc.client_site {
            r.log(format!("This computer's site: {site}"));
        }
        r.log(format!(
            "Using domain controller {} (port {}){}; {} of {} reachable",
            dc.host,
            dc.port,
            if dc.in_client_site { ", in this computer's site" } else { "" },
            disc.controllers.iter().filter(|c| c.reachable).count(),
            disc.controllers.len()
        ));
        let suffix = if disc.warnings.is_empty() {
            String::new()
        } else {
            format!(" ({} warning(s), see details)", disc.warnings.len())
        };
        let Some(offset) = disc.majority_clock_offset_secs else {
            return Ok((
                StepState::Done,
                format!(
                    "Found {}. Could not measure the domain's clock (time port blocked); continuing.{suffix}",
                    dc.host
                ),
                Some(dc.host.clone()),
            ));
        };
        r.log(format!("Clock difference agreed by most domain controllers: {offset:.1} s"));
        if offset.abs() <= CLOCK_OK_SECS {
            return Ok((
                StepState::Done,
                format!("Found {}. Clock is within {:.0} s of the domain.{suffix}", dc.host, offset.abs()),
                Some(dc.host.clone()),
            ));
        }
        r.log(format!(
            "This computer's clock is {:.0} s off; turning on time sync with {}.",
            offset.abs(),
            dc.host
        ));
        let ntp_actions = time_sync_actions(ctx.runner, &dc.host);
        execute_actions(ctx, &store, &mut state, r, &ntp_actions, Duration::from_secs(60))?;
        let deadline = Instant::now() + Duration::from_secs(45);
        let mut last = offset;
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_secs(3));
            if let Some(o) = net.clock_offset(&dc.host) {
                last = o;
                if o.abs() <= CLOCK_OK_SECS {
                    break;
                }
            }
        }
        if last.abs() > CLOCK_HARD_LIMIT_SECS {
            return Err(StepError(format!(
                "This computer's clock is {:.0} s away from the domain controller's and could not be corrected. Set the time, then try again.",
                last.abs()
            )));
        }
        Ok((
            StepState::Done,
            format!("Found {}. Clock corrected (now within {:.0} s).{suffix}", dc.host, last.abs()),
            Some(dc.host.clone()),
        ))
    })?;
    state.domain_controller = dc;

    // 4. join, through the recommended domain controller when there is one
    steps.run("join", JOIN_STEPS[3].1, |r| {
        for p in [
            "/etc/sssd/sssd.conf",
            "/etc/krb5.conf",
            "/etc/krb5.keytab",
            "/etc/nsswitch.conf",
            "/etc/pam.d/common-session",
            "/etc/pam.d/common-auth",
            "/etc/pam.d/common-account",
            "/etc/pam.d/common-password",
        ] {
            store
                .ensure_backup(&mut state, Path::new(p))
                .map_err(|e| StepError(format!("Could not back up {p}: {e}")))?;
        }
        let mut base = vec![
            "join".to_owned(),
            "--unattended".to_owned(),
            "--verbose".to_owned(),
            format!("--user={}", req.admin_user.trim()),
            "--membership-software=adcli".to_owned(),
        ];
        if let Some(ou) = req
            .computer_ou
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            base.push(format!("--computer-ou={ou}"));
        }
        if let Some(name) = req
            .computer_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            base.push(format!("--computer-name={name}"));
        }
        // Naming a domain controller instead of the domain makes realmd
        // discover the domain through that server and pass it to adcli
        // (`--domain-controller`), so the join cannot wander off to a DC
        // with a bad clock.
        let run_realm_join = |target: &str| -> Result<CommandOutput, StepError> {
            let mut args: Vec<&str> = base.iter().map(String::as_str).collect();
            args.push(target);
            let mut secret = Zeroizing::new(String::with_capacity(password.len() + 1));
            secret.push_str(&password);
            secret.push('\n');
            let spec = CommandSpec::new("realm", &args)
                .stdin(secret)
                .timeout(Duration::from_secs(10 * 60));
            let out = ctx
                .runner
                .run(&spec)
                .map_err(|e| StepError(format!("Could not run realm: {e}")))?;
            r.log_command(&spec.display(), &out);
            Ok(out)
        };
        let target = state.domain_controller.clone();
        let mut out = run_realm_join(target.as_deref().unwrap_or(&domain))?;
        if let Some(dc) = &target {
            if !out.success() && is_discovery_failure(&out.combined()) {
                r.log(format!(
                    "Warning: realm could not reach the domain through {dc}; letting it choose a domain controller for {domain} instead."
                ));
                out = run_realm_join(&domain)?;
            }
        }
        if !out.success() {
            let problem = explain_realm_join_failure(&out.combined());
            // realm can fail after adcli has already created the computer
            // account and written the local configuration.
            if system::joined_realm(ctx.runner).is_some() {
                mark_joined(&mut state, req, &info.hostname);
                state.setup_finished = Some(false);
                state.remaining_steps = POST_JOIN_STEPS.iter().map(|s| (*s).to_owned()).collect();
                *joined_now = true;
                store
                    .save(&state)
                    .map_err(|e| StepError(format!("Could not save Rabita's state: {e}")))?;
                return Err(StepError(format!(
                    "The computer was added to the domain, but realm then reported a problem: {}",
                    problem.trim_end_matches('.')
                )));
            }
            return Err(StepError(problem));
        }
        mark_joined(&mut state, req, &info.hostname);
        *joined_now = true;
        store
            .save(&state)
            .map_err(|e| StepError(format!("Could not save Rabita's state: {e}")))?;
        let message = match (used_domain_controller(&out.combined()), &target) {
            (Some(used), Some(wanted)) if !used.eq_ignore_ascii_case(wanted) => {
                r.log(format!(
                    "Warning: realm used domain controller {used} instead of the recommended {wanted}."
                ));
                format!("Joined {domain} using domain controller {used} (not the recommended {wanted}; see details).")
            }
            (Some(used), _) => format!("Joined {domain} using domain controller {used}."),
            (None, _) => format!("Joined {domain}."),
        };
        Ok((StepState::Done, message, ()))
    })?;
    drop(password);

    // 5-9. everything after the computer account exists
    let options = state.options.clone();
    let preferred_dc = state.domain_controller.clone();
    let pj = PostJoin {
        domain: &domain,
        options: &options,
        verify_user: req.verify_user.as_deref(),
        preferred_dc: preferred_dc.as_deref(),
        steps: &POST_JOIN_STEPS,
        undo_disabled: false,
        track_unfinished: true,
    };
    run_post_join(
        ctx,
        &store,
        &mut state,
        backend.as_ref(),
        info.display_manager,
        &pj,
        steps,
    )
}

/// Whether a package is installed, according to the package manager.
fn package_present(
    runner: &dyn Runner,
    family: rabita_backends::DistroFamily,
    package: &str,
) -> bool {
    match family {
        rabita_backends::DistroFamily::Debian => runner
            .run(&CommandSpec::new(
                "dpkg-query",
                &["-W", "-f=${Status}", package],
            ))
            .is_ok_and(|o| o.success() && o.stdout.contains("install ok installed")),
        // Other families are not implemented yet; always (re)install.
        _ => false,
    }
}

/// Actions that point the system's time sync at the domain controller.
#[must_use]
pub fn time_sync_actions(runner: &dyn Runner, dc_host: &str) -> Vec<Action> {
    let chrony_active = runner.which("chronyc")
        && runner
            .run(&CommandSpec::new("systemctl", &["is-active", "chrony"]))
            .is_ok_and(|o| o.stdout.trim() == "active");
    if chrony_active {
        vec![
            Action::write_file(
                "Add the domain controller as a time source",
                "/etc/chrony/sources.d/rabita.sources",
                &format!("server {dc_host} iburst\n"),
            ),
            Action::run_optional("Reload time sources", "chronyc", &["reload", "sources"]),
            Action::run_optional("Step the clock", "chronyc", &["makestep"]),
        ]
    } else {
        vec![
            Action::write_file(
                "Add the domain controller as a time source",
                "/etc/systemd/timesyncd.conf.d/90-rabita.conf",
                &format!("# Installed by Rabita\n[Time]\nNTP={dc_host}\n"),
            ),
            Action::run_optional(
                "Turn on automatic time",
                "timedatectl",
                &["set-ntp", "true"],
            ),
            Action::run(
                "Restart time sync",
                "systemctl",
                &["restart", "systemd-timesyncd"],
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::cmd::FakeRunner;
    use crate::progress::Collector;

    const KLIST: &str = "\
Keytab name: FILE:/etc/krb5.keytab
KVNO Principal
---- --------------------------------------------------------------------------
   2 host/lab-pc-01.lab.quietbyte.test@LAB.QUIETBYTE.TEST
   2 LAB-PC-01$@LAB.QUIETBYTE.TEST
";

    fn lab(dir: &Path) -> Paths {
        let paths = Paths::under(dir);
        let write = |abs: &str, content: &str| {
            let p = paths.resolve(Path::new(abs));
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, content).unwrap();
        };
        write(
            "/etc/os-release",
            "ID=ubuntu\nID_LIKE=debian\nPRETTY_NAME=\"Ubuntu 24.04\"\n",
        );
        write("/etc/hostname", "lab-pc-01\n");
        write(
            "/etc/pam.d/common-session",
            "session required pam_unix.so\n",
        );
        paths
    }

    fn request() -> JoinRequest {
        JoinRequest {
            domain: "LAB.QuietByte.test".into(),
            admin_user: "joiner".into(),
            computer_ou: Some("OU=Linux PCs,DC=lab,DC=quietbyte,DC=test".into()),
            computer_name: None,
            verify_user: Some("amna".into()),
            options: JoinOptions::default(),
        }
    }

    fn realmd_sssd_conf() -> &'static str {
        "[sssd]\ndomains = lab.quietbyte.test\nconfig_file_version = 2\nservices = nss, pam\n\n[domain/lab.quietbyte.test]\nid_provider = ad\nuse_fully_qualified_names = True\nfallback_homedir = /home/%u@%d\n"
    }

    #[test]
    fn happy_path_on_ubuntu_with_everything_installed() {
        let dir = tempfile::tempdir().unwrap();
        let paths = lab(dir.path());
        let fake = FakeRunner::new();
        fake.ok("realm list", "")
            .ok("dpkg-query", "install ok installed");
        // realm join "creates" sssd.conf as realmd would.
        let conf_path = paths.sssd_conf();
        fake.ok("realm join", "Successfully enrolled machine in realm");
        fake.ok("klist", KLIST)
            .ok("kinit", "")
            .ok("getent", "amna:*:1001:1001::/home/amna:/bin/bash");
        let collector = Collector::new();
        let ctx = JobContext {
            runner: &fake,
            paths: &paths,
            sink: &collector,
            network: None,
        };
        // Simulate realmd writing sssd.conf: write it before the job since the
        // fake runner has no side effects. Backups must still record it as the
        // pre-join version.
        fs::create_dir_all(conf_path.parent().unwrap()).unwrap();
        fs::write(&conf_path, realmd_sssd_conf()).unwrap();

        let result = run_join(&ctx, &request(), Zeroizing::new("Pa55w0rd!".into()));
        assert!(result.ok, "{}", result.summary);
        assert_eq!(
            result.summary,
            "This computer is now connected to lab.quietbyte.test."
        );

        // Password only ever on stdin, never argv; OU passed through.
        let join_call = fake
            .calls()
            .into_iter()
            .find(|c| c.program == "realm" && c.args.first().is_some_and(|a| a == "join"))
            .expect("realm join called");
        assert!(join_call.args.iter().all(|a| !a.contains("Pa55w0rd")));
        assert_eq!(
            join_call.stdin.as_deref().map(String::as_str),
            Some("Pa55w0rd!\n")
        );
        assert!(join_call.args.contains(&"--user=joiner".to_owned()));
        assert!(join_call
            .args
            .contains(&"--computer-ou=OU=Linux PCs,DC=lab,DC=quietbyte,DC=test".to_owned()));
        assert_eq!(
            join_call.args.last().map(String::as_str),
            Some("lab.quietbyte.test")
        );

        // sssd.conf edited as promised.
        let conf = fs::read_to_string(&conf_path).unwrap();
        assert!(conf.contains("use_fully_qualified_names = False"));
        assert!(conf.contains("fallback_homedir = /home/%u"));
        assert!(conf.contains("cache_credentials = True"));
        assert!(!conf.contains("default_domain_suffix"), "{conf}");
        assert!(!conf.contains("config_file_version"), "{conf}");

        // PAM and GDM steps planned (no display manager detected -> display skipped).
        let lines = fake.call_lines();
        assert!(lines
            .iter()
            .any(|l| l.starts_with("pam-auth-update --package --enable sss --enable")));
        let check = lines
            .iter()
            .position(|l| l == "sssctl config-check")
            .expect("sssctl config-check");
        let restart = lines
            .iter()
            .position(|l| l == "systemctl restart sssd")
            .expect("restart");
        assert!(check < restart, "config is checked before sssd restarts");
        assert!(lines
            .iter()
            .any(|l| l
                .starts_with("kinit -k -t /etc/krb5.keytab -c MEMORY:rabita-verify LAB-PC-01$@")));

        // State saved for leave.
        let state = Store::new(paths.clone()).load().unwrap();
        assert!(state.joined);
        assert_eq!(state.setup_finished, Some(true));
        assert_eq!(state.remaining_steps, Vec::<String>::new());
        assert!(!result.joined_not_finished);
        assert_eq!(state.domain.as_deref(), Some("lab.quietbyte.test"));
        assert_eq!(state.computer_name.as_deref(), Some("LAB-PC-01"));
        assert!(state
            .backups
            .iter()
            .any(|b| b.original == Path::new("/etc/sssd/sssd.conf") && b.existed));
        assert!(state
            .backups
            .iter()
            .any(|b| b.original == Path::new("/etc/krb5.keytab") && !b.existed));

        let ids: Vec<&str> = result.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            ["detect", "packages", "time", "join", "sssd", "access", "home", "display", "verify"]
        );
        assert_eq!(result.steps[1].state, StepState::Skipped); // packages present
        assert_eq!(result.steps[5].state, StepState::Done);
        assert_eq!(result.steps[5].message, "All domain users");
        assert!(lines.contains(&"realm permit --all".to_owned()));
        assert_eq!(result.steps[7].state, StepState::Skipped); // no DM
        assert_eq!(result.steps[8].state, StepState::Done);
    }

    #[test]
    fn group_restricted_login_uses_realm_permit() {
        let dir = tempfile::tempdir().unwrap();
        let paths = lab(dir.path());
        let fake = FakeRunner::new();
        fake.ok("realm list", "")
            .ok("dpkg-query", "install ok installed")
            .ok("realm join", "")
            .ok("klist", KLIST);
        let collector = Collector::new();
        let ctx = JobContext {
            runner: &fake,
            paths: &paths,
            sink: &collector,
            network: None,
        };
        let mut req = request();
        req.verify_user = None;
        req.options.login_access = LoginAccess::Groups {
            groups: vec!["Linux Users".into(), " Staff ".into()],
        };
        let result = run_join(&ctx, &req, Zeroizing::new("pw".into()));
        assert!(result.ok, "{}", result.summary);
        let lines = fake.call_lines();
        let deny = lines
            .iter()
            .position(|l| l == "realm deny --all")
            .expect("realm deny --all");
        let permit = lines
            .iter()
            .position(|l| l == "realm permit -g Linux Users Staff")
            .expect("realm permit -g");
        assert!(deny < permit, "deny must come before permit");
        let sssd_restart = lines
            .iter()
            .position(|l| l == "systemctl restart sssd")
            .unwrap();
        assert!(
            sssd_restart < deny,
            "sssd must be configured before permissions"
        );
        assert!(!lines.contains(&"realm permit --all".to_owned()));
        // The group name with a space is one argument, not two.
        let permit_call = fake
            .calls()
            .into_iter()
            .find(|c| c.args.first().is_some_and(|a| a == "permit"))
            .unwrap();
        assert_eq!(permit_call.args, ["permit", "-g", "Linux Users", "Staff"]);
        let access = result.steps.iter().find(|s| s.id == "access").unwrap();
        assert_eq!(access.message, "Only these groups: Linux Users, Staff");
        let state = Store::new(paths).load().unwrap();
        assert_eq!(
            state.options.login_access,
            LoginAccess::Groups {
                groups: vec!["Linux Users".into(), "Staff".into()]
            }
        );
    }

    #[test]
    fn empty_group_list_is_rejected_before_any_change() {
        let dir = tempfile::tempdir().unwrap();
        let paths = lab(dir.path());
        let fake = FakeRunner::new();
        let collector = Collector::new();
        let ctx = JobContext {
            runner: &fake,
            paths: &paths,
            sink: &collector,
            network: None,
        };
        let mut req = request();
        req.options.login_access = LoginAccess::Groups {
            groups: vec!["  ".into()],
        };
        let result = run_join(&ctx, &req, Zeroizing::new("pw".into()));
        assert!(!result.ok);
        assert_eq!(
            result.summary,
            "Enter at least one group, or choose 'All domain users'."
        );
        assert!(
            fake.calls().is_empty(),
            "nothing may run: {:?}",
            fake.call_lines()
        );
    }

    #[test]
    fn login_access_actions_shapes() {
        let all = login_access_actions(&LoginAccess::All).unwrap();
        assert_eq!(all.len(), 1);
        let groups = login_access_actions(&LoginAccess::Groups {
            groups: vec!["A".into()],
        })
        .unwrap();
        assert_eq!(groups.len(), 2);
        assert!(login_access_actions(&LoginAccess::Groups { groups: vec![] }).is_err());
    }

    #[test]
    fn wrong_password_is_explained_and_later_steps_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let paths = lab(dir.path());
        let fake = FakeRunner::new();
        fake.ok("realm list", "").fail(
            "realm join",
            "realm: Couldn't authenticate as joiner: Preauthentication failed",
        );
        let collector = Collector::new();
        let ctx = JobContext {
            runner: &fake,
            paths: &paths,
            sink: &collector,
            network: None,
        };
        let result = run_join(&ctx, &request(), Zeroizing::new("bad".into()));
        assert!(!result.ok);
        assert_eq!(
            result.summary,
            "The administrator username or password was not accepted."
        );
        let join = result.steps.iter().find(|s| s.id == "join").unwrap();
        assert_eq!(join.state, StepState::Failed);
        let verify = result.steps.iter().find(|s| s.id == "verify").unwrap();
        assert_eq!(verify.state, StepState::Skipped);
        assert!(!Store::new(paths).load().unwrap().joined);
        assert!(!fake
            .call_lines()
            .iter()
            .any(|l| l.starts_with("systemctl restart sssd")));
    }

    #[test]
    fn installs_missing_packages_noninteractively() {
        let dir = tempfile::tempdir().unwrap();
        let paths = lab(dir.path());
        let fake = FakeRunner::new();
        fake.installed(&[
            "apt-get",
            "debconf-set-selections",
            "systemctl",
            "klist",
            "kinit",
            "getent",
            "pam-auth-update",
        ]);
        // After apt-get install, realm/adcli/sssd still missing -> clear error.
        let collector = Collector::new();
        let ctx = JobContext {
            runner: &fake,
            paths: &paths,
            sink: &collector,
            network: None,
        };
        let result = run_join(&ctx, &request(), Zeroizing::new("x".into()));
        assert!(!result.ok);
        assert!(
            result.summary.contains("still missing"),
            "{}",
            result.summary
        );
        let lines = fake.call_lines();
        let apt = lines
            .iter()
            .find(|l| l.starts_with("apt-get install"))
            .expect("apt-get install");
        assert!(
            apt.contains("realmd")
                && apt.contains("sssd")
                && apt.contains("adcli")
                && apt.contains("libpam-sss")
        );
        let apt_call = fake
            .calls()
            .into_iter()
            .find(|c| c.program == "apt-get" && c.args[0] == "install")
            .unwrap();
        assert!(apt_call
            .env
            .iter()
            .any(|(k, v)| k == "DEBIAN_FRONTEND" && v == "noninteractive"));
    }

    #[test]
    fn refuses_unsupported_or_already_joined() {
        let dir = tempfile::tempdir().unwrap();
        let paths = lab(dir.path());
        let fake = FakeRunner::new();
        fake.ok("realm list", "lab.quietbyte.test\n  domain-name: lab.quietbyte.test\n  configured: kerberos-member\n");
        let collector = Collector::new();
        let ctx = JobContext {
            runner: &fake,
            paths: &paths,
            sink: &collector,
            network: None,
        };
        let result = run_join(&ctx, &request(), Zeroizing::new("x".into()));
        assert!(!result.ok);
        assert!(result.summary.contains("already connected"));
        assert!(!fake
            .call_lines()
            .iter()
            .any(|l| l.starts_with("realm join")));
    }

    #[test]
    fn explains_common_realm_errors() {
        assert!(
            explain_realm_join_failure("Insufficient permissions to join the domain")
                .contains("not allowed")
        );
        assert!(
            explain_realm_join_failure("No such realm found: foo").contains("could not be found")
        );
        assert!(explain_realm_join_failure("Clock skew too great").contains("clock"));
        assert!(explain_realm_join_failure("something odd\nlast line")
            .ends_with("something odd / last line"));
    }

    #[test]
    fn configures_sssd_text() {
        let out = configure_sssd_text(realmd_sssd_conf(), "lab.quietbyte.test", false, None);
        assert!(out.contains("[sssd]\ndomains = lab.quietbyte.test\nservices = nss, pam\n\n"));
        assert!(out.contains("cache_credentials = False"));
        assert!(out.contains("ad_gpo_access_control = permissive"));
        assert!(!out.contains("ad_server"), "no preferred DC, no ad_server");
        assert_eq!(out.matches("use_fully_qualified_names").count(), 1);
    }

    /// Lab bug: SSSD 2.10+ refuses to start with `default_domain_suffix`
    /// plus `use_fully_qualified_names = False`. Both it and
    /// `config_file_version` must go, even if an older Rabita wrote them.
    #[test]
    fn sssd_conf_drops_suffix_and_version_and_prefers_the_dc() {
        let old = "[sssd]\ndomains = lab.quietbyte.test\nconfig_file_version = 2\nservices = nss, pam\ndefault_domain_suffix = lab.quietbyte.test\n\n[domain/lab.quietbyte.test]\nid_provider = ad\nuse_fully_qualified_names = False\ndefault_domain_suffix = lab.quietbyte.test\n";
        assert!(sssd_conf_needs_repair(old));
        let out = configure_sssd_text(
            old,
            "lab.quietbyte.test",
            true,
            Some("bdc-2021.lab.quietbyte.test"),
        );
        assert!(!out.contains("default_domain_suffix"), "{out}");
        assert!(!out.contains("config_file_version"), "{out}");
        assert!(!sssd_conf_needs_repair(&out));
        assert!(out.contains("use_fully_qualified_names = False\n"));
        assert!(out.contains("ad_server = bdc-2021.lab.quietbyte.test, _srv_\n"));
        // Running it again changes nothing.
        assert_eq!(
            configure_sssd_text(
                &out,
                "lab.quietbyte.test",
                true,
                Some("bdc-2021.lab.quietbyte.test")
            ),
            out
        );
    }

    fn output(code: i32, stdout: &str) -> CommandOutput {
        CommandOutput {
            code: Some(code),
            stdout: stdout.to_owned(),
            ..CommandOutput::default()
        }
    }

    #[test]
    fn reads_sssctl_config_check_results() {
        let clean = "Issues identified by validators: 0\n\nMessages generated during configuration merging: 0\n\nUsed configuration snippet files: 0\n";
        assert_eq!(sssctl_problems(&output(0, clean)), Vec::<String>::new());
        let bad = "Issues identified by validators: 1\n[rule/allowed_sssd_options]: Attribute 'default_domain_suffx' is not allowed in section 'sssd'. Check for typos.\n\nMessages generated during configuration merging: 0\n";
        let expected = ["[rule/allowed_sssd_options]: Attribute 'default_domain_suffx' is not allowed in section 'sssd'. Check for typos."];
        assert_eq!(sssctl_problems(&output(1, bad)), expected);
        // Some sssctl versions exit 0 even when validators complain.
        assert_eq!(sssctl_problems(&output(0, bad)), expected);
        let perms = "File ownership and permissions check failed. Expected root:root and 0600.\n";
        assert_eq!(sssctl_problems(&output(1, perms)).len(), 1);
        // Failure with nothing recognisable still counts, with its last lines.
        assert_eq!(
            sssctl_problems(&output(3, "odd\nfailure\n")),
            ["odd / failure"]
        );
    }

    #[test]
    fn reads_which_dc_adcli_used() {
        let out = " * Resolving: _ldap._tcp.lab.quietbyte.test\n * Using domain controller: bdc-2021.lab.quietbyte.test\n * Successfully enrolled machine in realm\n";
        assert_eq!(
            used_domain_controller(out).as_deref(),
            Some("bdc-2021.lab.quietbyte.test")
        );
        assert_eq!(used_domain_controller("Successfully enrolled"), None);
        assert!(is_discovery_failure(
            "realm: No such realm found: bdc-2021.lab.quietbyte.test"
        ));
        assert!(!is_discovery_failure("Couldn't authenticate as joiner"));
    }

    /// Replays the lab domain: BDC-2021 is healthy; DC-B1-4 is reachable
    /// but its clock is 167 s off; two old records are dead.
    struct LabNetwork;

    impl Network for LabNetwork {
        fn discover(&self, _: &str, _: &dyn Runner, _: &Paths) -> crate::types::DiscoveryResult {
            let dc =
                |host: &str, reachable: bool, offset: Option<f64>| crate::types::DomainController {
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
                dc("dc-b1-4.lab.quietbyte.test", true, Some(167.0)),
                dc("bdc-2021.lab.quietbyte.test", true, Some(1.0)),
                dc("dc-a.lab.quietbyte.test", true, Some(3.0)),
                dc("dc-b.lab.quietbyte.test", true, Some(-2.0)),
                dc("old-dc1.lab.quietbyte.test", false, None),
                dc("old-dc2.lab.quietbyte.test", false, None),
            ];
            let ranking = discover::rank_controllers(&mut controllers, &[]);
            crate::types::DiscoveryResult {
                domain: "lab.quietbyte.test".into(),
                realm: "LAB.QUIETBYTE.TEST".into(),
                found: true,
                controllers,
                problem: None,
                client_site: None,
                recommended: ranking.recommended,
                majority_clock_offset_secs: ranking.majority_offset,
                warnings: ranking.warnings,
            }
        }

        fn clock_offset(&self, _: &str) -> Option<f64> {
            None
        }
    }

    fn logs(c: &Collector) -> Vec<String> {
        c.events()
            .into_iter()
            .filter_map(|e| match e {
                crate::progress::ProgressEvent::Log(l) => Some(l),
                crate::progress::ProgressEvent::Step { .. } => None,
            })
            .collect()
    }

    fn realm_join_calls(fake: &FakeRunner) -> Vec<CommandSpec> {
        fake.calls()
            .into_iter()
            .filter(|c| c.program == "realm" && c.args.first().is_some_and(|a| a == "join"))
            .collect()
    }

    /// Lab bug: Rabita recommended BDC-2021 but realm picked DC-B1-4
    /// (167 s drift) itself. The join must go through the recommended DC.
    #[test]
    fn joins_through_the_recommended_dc() {
        let dir = tempfile::tempdir().unwrap();
        let paths = lab(dir.path());
        let fake = FakeRunner::new();
        fake.ok("realm list", "")
            .ok("dpkg-query", "install ok installed")
            .ok(
                "realm join",
                " * Using domain controller: bdc-2021.lab.quietbyte.test\n * Successfully enrolled machine in realm\n",
            )
            .ok("klist", KLIST)
            .ok("getent", "amna:*:1001:1001::/home/amna:/bin/bash");
        let collector = Collector::new();
        let ctx = JobContext {
            runner: &fake,
            paths: &paths,
            sink: &collector,
            network: Some(&LabNetwork),
        };
        let result = run_join(&ctx, &request(), Zeroizing::new("pw".into()));
        assert!(result.ok, "{}", result.summary);

        let joins = realm_join_calls(&fake);
        assert_eq!(joins.len(), 1);
        assert_eq!(
            joins[0].args.last().map(String::as_str),
            Some("bdc-2021.lab.quietbyte.test"),
            "realm must be pointed at the recommended DC, not left to choose"
        );
        let time = result.steps.iter().find(|s| s.id == "time").unwrap();
        assert!(
            time.message
                .starts_with("Found bdc-2021.lab.quietbyte.test."),
            "{}",
            time.message
        );
        let join = result.steps.iter().find(|s| s.id == "join").unwrap();
        assert_eq!(
            join.message,
            "Joined lab.quietbyte.test using domain controller bdc-2021.lab.quietbyte.test."
        );
        // SSSD keeps preferring it afterwards.
        let conf = fs::read_to_string(paths.sssd_conf()).unwrap();
        assert!(
            conf.contains("ad_server = bdc-2021.lab.quietbyte.test, _srv_"),
            "{conf}"
        );
        let state = Store::new(paths).load().unwrap();
        assert_eq!(
            state.domain_controller.as_deref(),
            Some("bdc-2021.lab.quietbyte.test")
        );
        // The majority clock is fine, so nothing touched time sync.
        assert!(!fake
            .call_lines()
            .iter()
            .any(|l| l.contains("timesyncd") || l.starts_with("chronyc")));
    }

    #[test]
    fn falls_back_to_the_domain_when_realm_cannot_use_the_dc() {
        let dir = tempfile::tempdir().unwrap();
        let paths = lab(dir.path());
        let fake = FakeRunner::new();
        fake.ok("realm list", "")
            .ok("dpkg-query", "install ok installed")
            .fail(
                "realm join",
                "realm: No such realm found: bdc-2021.lab.quietbyte.test",
            )
            .ok(
                "realm join",
                " * Using domain controller: dc-a.lab.quietbyte.test\n",
            )
            .ok("klist", KLIST)
            .ok("getent", "amna:*:1001:1001::/home/amna:/bin/bash");
        let collector = Collector::new();
        let ctx = JobContext {
            runner: &fake,
            paths: &paths,
            sink: &collector,
            network: Some(&LabNetwork),
        };
        let result = run_join(&ctx, &request(), Zeroizing::new("pw".into()));
        assert!(result.ok, "{}", result.summary);
        let joins = realm_join_calls(&fake);
        assert_eq!(joins.len(), 2);
        assert_eq!(
            joins[0].args.last().map(String::as_str),
            Some("bdc-2021.lab.quietbyte.test")
        );
        assert_eq!(
            joins[1].args.last().map(String::as_str),
            Some("lab.quietbyte.test")
        );
        assert_eq!(joins[1].stdin.as_deref().map(String::as_str), Some("pw\n"));
        assert!(logs(&collector)
            .iter()
            .any(|l| l.contains("letting it choose a domain controller")));
        let join = result.steps.iter().find(|s| s.id == "join").unwrap();
        assert!(
            join.message
                .contains("not the recommended bdc-2021.lab.quietbyte.test"),
            "{}",
            join.message
        );
    }

    /// Lab bug: a step after the join failed and nothing said so; later
    /// steps never ran and the user got in without a home folder.
    #[test]
    fn config_check_errors_leave_the_computer_joined_but_not_finished() {
        let dir = tempfile::tempdir().unwrap();
        let paths = lab(dir.path());
        fs::create_dir_all(paths.sssd_conf().parent().unwrap()).unwrap();
        fs::write(paths.sssd_conf(), realmd_sssd_conf()).unwrap();
        let fake = FakeRunner::new();
        fake.ok("realm list", "")
            .ok("dpkg-query", "install ok installed")
            .ok("realm join", "")
            .respond(
                "sssctl config-check",
                output(
                    1,
                    "Issues identified by validators: 1\n[rule/allowed_domain_options]: Attribute 'ad_servr' is not allowed in section 'domain/lab.quietbyte.test'. Check for typos.\n",
                ),
            );
        let collector = Collector::new();
        let ctx = JobContext {
            runner: &fake,
            paths: &paths,
            sink: &collector,
            network: None,
        };
        let result = run_join(&ctx, &request(), Zeroizing::new("pw".into()));
        assert!(!result.ok);
        assert!(result.joined_not_finished);
        assert_eq!(result.remaining_steps, POST_JOIN_STEPS);
        assert!(result.summary.starts_with(
            "This computer is joined to lab.quietbyte.test but not finished. The new sign-in settings in /etc/sssd/sssd.conf have errors, so the sign-in service was not restarted: [rule/allowed_domain_options]: Attribute 'ad_servr'"
        ), "{}", result.summary);
        assert!(result.summary.ends_with(
            "run Finish setup (rabitactl finish) to complete the remaining steps: Set up sign-in, Choose who can sign in, Create home folders on first login, Show a username box on the login screen, Check that it works."
        ), "{}", result.summary);
        let lines = fake.call_lines();
        assert!(
            !lines.iter().any(|l| l == "systemctl restart sssd"),
            "bad config must not be started"
        );
        assert!(!lines
            .iter()
            .any(|l| l.starts_with("pam-auth-update") || l.starts_with("realm permit")));
        let state_of = |id: &str| result.steps.iter().find(|s| s.id == id).unwrap().clone();
        assert_eq!(state_of("sssd").state, StepState::Failed);
        assert_eq!(state_of("home").state, StepState::Skipped);
        assert_eq!(
            state_of("home").message,
            "Not run yet. Finish setup will run it."
        );
        let state = Store::new(paths.clone()).load().unwrap();
        assert!(state.joined);
        assert_eq!(state.setup_finished, Some(false));
        assert_eq!(state.remaining_steps, POST_JOIN_STEPS);

        // Once the problem is fixed, Finish setup (ApplyOptions with the
        // saved options) runs every remaining step.
        let fixed = FakeRunner::new();
        fixed
            .ok(
                "realm list",
                "lab.quietbyte.test\n  configured: kerberos-member\n",
            )
            .ok("klist", KLIST)
            .ok("getent", "amna:*:1001:1001::/home/amna:/bin/bash");
        let ctx = JobContext {
            runner: &fixed,
            paths: &paths,
            sink: &collector,
            network: None,
        };
        let done = crate::service::apply_options_job(&ctx, &state.options);
        assert!(done.ok, "{}", done.summary);
        assert!(!done.joined_not_finished);
        assert_eq!(
            done.summary,
            "Setup finished. This computer is fully connected to lab.quietbyte.test."
        );
        let ids: Vec<&str> = done.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, POST_JOIN_STEPS);
        let lines = fixed.call_lines();
        for expected in [
            "sssctl config-check",
            "systemctl restart sssd",
            "realm permit --all",
        ] {
            assert!(
                lines.contains(&expected.to_owned()),
                "missing {expected}: {lines:?}"
            );
        }
        assert!(lines
            .iter()
            .any(|l| l.starts_with("pam-auth-update --package --enable sss")));
        assert!(lines.iter().any(|l| l.starts_with("kinit -k")));
        let state = Store::new(paths.clone()).load().unwrap();
        assert_eq!(state.setup_finished, Some(true));
        assert_eq!(state.remaining_steps, Vec::<String>::new());
        assert!(!fs::read_to_string(paths.sssd_conf())
            .unwrap()
            .contains("config_file_version"));
    }

    #[test]
    fn a_later_failure_lists_only_the_steps_still_to_run() {
        let dir = tempfile::tempdir().unwrap();
        let paths = lab(dir.path());
        let fake = FakeRunner::new();
        fake.ok("realm list", "")
            .ok("dpkg-query", "install ok installed")
            .ok("realm join", "")
            .fail("pam-auth-update", "pam-auth-update: profile not found");
        let collector = Collector::new();
        let ctx = JobContext {
            runner: &fake,
            paths: &paths,
            sink: &collector,
            network: None,
        };
        let result = run_join(&ctx, &request(), Zeroizing::new("pw".into()));
        assert!(!result.ok && result.joined_not_finished);
        assert_eq!(result.remaining_steps, ["home", "display", "verify"]);
        assert!(
            result
                .summary
                .contains("pam-auth-update: profile not found"),
            "{}",
            result.summary
        );
        assert!(result.summary.ends_with(
            "remaining steps: Create home folders on first login, Show a username box on the login screen, Check that it works."
        ));
        let state_of = |id: &str| result.steps.iter().find(|s| s.id == id).unwrap().state;
        assert_eq!(state_of("sssd"), StepState::Done);
        assert_eq!(state_of("access"), StepState::Done);
        assert_eq!(state_of("home"), StepState::Failed);
        assert_eq!(state_of("verify"), StepState::Skipped);
        let state = Store::new(paths).load().unwrap();
        assert_eq!(state.remaining_steps, ["home", "display", "verify"]);
    }

    #[test]
    fn realm_failing_after_adding_the_computer_is_joined_but_not_finished() {
        let dir = tempfile::tempdir().unwrap();
        let paths = lab(dir.path());
        let fake = FakeRunner::new();
        // Not a member when the job starts; a member after realm failed.
        fake.ok("realm list", "")
            .ok(
                "realm list",
                "lab.quietbyte.test\n  configured: kerberos-member\n",
            )
            .ok("dpkg-query", "install ok installed")
            .fail("realm join", "realm: Couldn't start the sssd service");
        let collector = Collector::new();
        let ctx = JobContext {
            runner: &fake,
            paths: &paths,
            sink: &collector,
            network: None,
        };
        let result = run_join(&ctx, &request(), Zeroizing::new("pw".into()));
        assert!(!result.ok && result.joined_not_finished);
        assert_eq!(result.remaining_steps, POST_JOIN_STEPS);
        assert!(result.summary.starts_with(
            "This computer is joined to lab.quietbyte.test but not finished. The computer was added to the domain, but realm then reported a problem"
        ), "{}", result.summary);
        let state = Store::new(paths).load().unwrap();
        assert!(state.joined);
        assert_eq!(state.setup_finished, Some(false));
    }

    #[test]
    fn a_second_join_points_an_unfinished_computer_to_finish_setup() {
        let dir = tempfile::tempdir().unwrap();
        let paths = lab(dir.path());
        Store::new(paths.clone())
            .save(&State {
                domain: Some("lab.quietbyte.test".into()),
                joined: true,
                setup_finished: Some(false),
                remaining_steps: vec!["home".into()],
                ..State::default()
            })
            .unwrap();
        let fake = FakeRunner::new();
        let collector = Collector::new();
        let ctx = JobContext {
            runner: &fake,
            paths: &paths,
            sink: &collector,
            network: None,
        };
        let result = run_join(&ctx, &request(), Zeroizing::new("pw".into()));
        assert!(!result.ok);
        assert_eq!(
            result.summary,
            "This computer is already joined to lab.quietbyte.test but not finished. Run Finish setup (rabitactl finish) to complete it, or Leave domain to start again."
        );
        assert_eq!(realm_join_calls(&fake).len(), 0);
    }

    #[test]
    fn picks_machine_account_principal() {
        assert_eq!(
            keytab_principal(KLIST).as_deref(),
            Some("LAB-PC-01$@LAB.QUIETBYTE.TEST")
        );
        assert_eq!(
            keytab_principal("Keytab name: FILE:/etc/krb5.keytab\nKVNO Principal\n"),
            None
        );
        assert_eq!(
            keytab_principal("   3 host/x.test@X.TEST\n").as_deref(),
            Some("host/x.test@X.TEST")
        );
    }

    #[test]
    fn time_sync_prefers_chrony_when_active() {
        let fake = FakeRunner::new();
        fake.ok("systemctl is-active", "active\n");
        let actions = time_sync_actions(&fake, "dc1.lab.test");
        assert!(
            matches!(&actions[0], Action::WriteFile { path, content, .. } if path == Path::new("/etc/chrony/sources.d/rabita.sources") && content.contains("server dc1.lab.test iburst"))
        );
        let fake = FakeRunner::new();
        fake.installed(&["systemctl", "timedatectl"]);
        let actions = time_sync_actions(&fake, "dc1.lab.test");
        assert!(
            matches!(&actions[0], Action::WriteFile { path, content, .. } if path == Path::new("/etc/systemd/timesyncd.conf.d/90-rabita.conf") && content.contains("NTP=dc1.lab.test"))
        );
    }
}
