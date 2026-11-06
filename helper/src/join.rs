// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! The `Join` job: connect this computer to the organization.
//!
//! Steps (each reported live, each reversible through the backup store):
//! 1. detect   — is this computer supported?
//! 2. packages — install realmd, sssd, adcli and friends
//! 3. time     — find the domain controllers and check the clock
//! 4. join     — `realm join` with the admin password on stdin
//! 5. sssd     — plain usernames, offline login, `/home/<user>`
//! 6. access   — who may sign in (everyone, or only listed groups)
//! 7. home     — create home folders on first login
//! 8. display  — username box on the login screen
//! 9. verify   — Kerberos ticket from the machine keytab, user lookup

use std::path::Path;
use std::time::{Duration, Instant};

use rabita_backends::{Action, Backend, SystemFacts};
use zeroize::Zeroizing;

use crate::backup::{State, Store};
use crate::cmd::{CommandSpec, Runner};
use crate::discover;
use crate::ini::IniDocument;
use crate::paths::Paths;
use crate::progress::{ProgressSink, Reporter};
use crate::sntp;
use crate::system;
use crate::types::{JobResult, JoinRequest, LoginAccess, StepState};

/// Everything a job needs from its surroundings.
pub struct JobContext<'a> {
    /// Runs system tools.
    pub runner: &'a dyn Runner,
    /// Where files live.
    pub paths: &'a Paths,
    /// Receives progress.
    pub sink: &'a dyn ProgressSink,
    /// Whether to probe the network (DNS, SNTP). Tests turn this off.
    pub network: bool,
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
/// login, `/home/<user>`.
#[must_use]
pub fn configure_sssd_text(text: &str, domain: &str, offline_login: bool) -> String {
    let mut doc = IniDocument::parse(text);
    doc.set("sssd", "default_domain_suffix", domain);
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
    doc.render()
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

struct Steps<'a> {
    reporter: Reporter<'a>,
}

impl Steps<'_> {
    fn run<T>(
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
    let steps = Steps {
        reporter: Reporter::new(&tee),
    };
    let outcome = join_inner(ctx, req, password, &steps);
    let mut steps_out = collector.outcomes();
    if outcome.is_err() {
        for s in &mut steps_out {
            if s.state == StepState::Pending {
                s.state = StepState::Skipped;
                "Not run because an earlier step failed.".clone_into(&mut s.message);
            }
        }
    }
    let (domain, _) = discover::normalise_domain(&req.domain);
    JobResult {
        ok: outcome.is_ok(),
        summary: match outcome {
            Ok(()) => format!("This computer is now connected to {domain}."),
            Err(e) => e.0,
        },
        steps: steps_out,
    }
}

#[allow(clippy::too_many_lines)] // one step after another; splitting would hide the order
fn join_inner(
    ctx: &JobContext<'_>,
    req: &JoinRequest,
    password: Zeroizing<String>,
    steps: &Steps<'_>,
) -> Result<(), StepError> {
    let store = Store::new(ctx.paths.clone());
    let (domain, realm) = discover::normalise_domain(&req.domain);
    if !discover::looks_like_domain(&domain) {
        steps.run("detect", JOIN_STEPS[0].1, |_| {
            Err(StepError::from(
                "Enter the full domain name, for example school.example.org.",
            ))
        })?;
    }
    if req.admin_user.trim().is_empty() {
        steps.run("detect", JOIN_STEPS[0].1, |_| {
            Err(StepError::from("Enter the administrator account name."))
        })?;
    }
    let access_actions = match login_access_actions(&req.options.login_access) {
        Ok(a) => a,
        Err(e) => {
            steps.run("detect", JOIN_STEPS[0].1, |_| {
                Err::<(StepState, String, ()), _>(e)
            })?;
            unreachable!("step failure returns early")
        }
    };

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
        for tool in ["realm", "adcli", "sssd"] {
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
        if !ctx.network {
            return Ok((StepState::Skipped, "Network checks disabled.".to_owned(), None));
        }
        let disc = discover::discover(&domain, ctx.paths, true);
        if let Some(p) = &disc.problem {
            if !disc.found || !disc.controllers.iter().any(|c| c.reachable) {
                return Err(StepError(p.clone()));
            }
        }
        let dc = disc
            .controllers
            .iter()
            .find(|c| c.reachable)
            .cloned()
            .ok_or("No domain controller could be reached.")?;
        r.log(format!("Domain controller: {} (port {})", dc.host, dc.port));
        let Some(offset) = dc.clock_offset_secs else {
            return Ok((
                StepState::Done,
                format!("Found {}. Could not measure its clock (time port blocked); continuing.", dc.host),
                Some(dc.host.clone()),
            ));
        };
        r.log(format!("Clock difference: {offset:.1} s"));
        if offset.abs() <= CLOCK_OK_SECS {
            return Ok((
                StepState::Done,
                format!("Found {}. Clock is within {:.0} s.", dc.host, offset.abs()),
                Some(dc.host.clone()),
            ));
        }
        r.log("Clock is too far off; turning on time sync with the domain controller.");
        let ntp_actions = time_sync_actions(ctx.runner, &dc.host);
        execute_actions(ctx, &store, &mut state, r, &ntp_actions, Duration::from_secs(60))?;
        let deadline = Instant::now() + Duration::from_secs(45);
        let mut last = offset;
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_secs(3));
            if let Ok(o) = sntp::clock_offset(&dc.host, Duration::from_secs(3)) {
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
            format!("Found {}. Clock corrected (now within {:.0} s).", dc.host, last.abs()),
            Some(dc.host.clone()),
        ))
    })?;
    state.domain_controller = dc;

    // 4. join
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
        let mut args = vec![
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
            args.push(format!("--computer-ou={ou}"));
        }
        if let Some(name) = req
            .computer_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            args.push(format!("--computer-name={name}"));
        }
        args.push(domain.clone());
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut secret = Zeroizing::new(String::with_capacity(password.len() + 1));
        secret.push_str(&password);
        secret.push('\n');
        let spec = CommandSpec::new("realm", &arg_refs)
            .stdin(secret)
            .timeout(Duration::from_secs(10 * 60));
        let display = spec.display();
        let out = ctx
            .runner
            .run(&spec)
            .map_err(|e| StepError(format!("Could not run realm: {e}")))?;
        r.log_command(&display, &out);
        if !out.success() {
            return Err(StepError(explain_realm_join_failure(&out.combined())));
        }
        state.joined = true;
        state.joined_at = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        );
        state.computer_name = Some(req.computer_name.clone().unwrap_or_else(|| {
            info.hostname
                .split('.')
                .next()
                .unwrap_or("")
                .to_ascii_uppercase()
        }));
        store
            .save(&state)
            .map_err(|e| StepError(format!("Could not save Rabita's state: {e}")))?;
        Ok((StepState::Done, format!("Joined {domain}."), ()))
    })?;
    drop(password);

    // 5. sssd
    steps.run("sssd", JOIN_STEPS[4].1, |r| {
        let conf_path = ctx.paths.sssd_conf();
        let text = std::fs::read_to_string(&conf_path).unwrap_or_default();
        let new_text = configure_sssd_text(&text, &domain, req.options.offline_login);
        store
            .write_file(
                &mut state,
                Path::new("/etc/sssd/sssd.conf"),
                &new_text,
                0o600,
            )
            .map_err(|e| StepError(format!("Could not update sssd.conf: {e}")))?;
        r.log("Updated /etc/sssd/sssd.conf: plain usernames, offline login, /home/<user>");
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
        execute_actions(
            ctx,
            &store,
            &mut state,
            r,
            &actions,
            Duration::from_secs(60),
        )?;
        Ok((StepState::Done, "Sign-in configured.".to_owned(), ()))
    })?;

    // 6. access
    steps.run("access", JOIN_STEPS[5].1, |r| {
        execute_actions(
            ctx,
            &store,
            &mut state,
            r,
            &access_actions,
            Duration::from_secs(120),
        )?;
        Ok((StepState::Done, state.options.login_access.describe(), ()))
    })?;

    // 7. home
    let facts = SystemFacts {
        mkhomedir_already_enabled: state.mkhomedir_was_enabled,
        ..system::gather_facts(ctx.paths)
    };
    steps.run("home", JOIN_STEPS[6].1, |r| {
        if !req.options.create_home {
            return Ok((StepState::Skipped, "Turned off in options.".to_owned(), ()));
        }
        let actions = backend.enable_login_integration(&facts);
        if actions.is_empty() {
            return Ok((
                StepState::Skipped,
                "Already enabled on this computer.".to_owned(),
                (),
            ));
        }
        execute_actions(
            ctx,
            &store,
            &mut state,
            r,
            &actions,
            Duration::from_secs(60),
        )?;
        Ok((
            StepState::Done,
            "Home folders will be created on first login.".to_owned(),
            (),
        ))
    })?;

    // 8. display
    steps.run("display", JOIN_STEPS[7].1, |r| {
        if !req.options.manual_login {
            return Ok((StepState::Skipped, "Turned off in options.".to_owned(), ()));
        }
        let actions = backend.enable_manual_login(&facts);
        if actions.is_empty() {
            return Ok((
                StepState::Skipped,
                format!(
                    "No change needed for this login screen ({:?}).",
                    info.display_manager
                ),
                (),
            ));
        }
        execute_actions(
            ctx,
            &store,
            &mut state,
            r,
            &actions,
            Duration::from_secs(60),
        )?;
        Ok((StepState::Done, "Login screen updated.".to_owned(), ()))
    })?;
    store
        .save(&state)
        .map_err(|e| StepError(format!("Could not save Rabita's state: {e}")))?;

    // 9. verify
    steps.run("verify", JOIN_STEPS[8].1, |r| {
        let mut notes = Vec::new();
        let klist = ctx
            .runner
            .run(&CommandSpec::new("klist", &["-k", "/etc/krb5.keytab"]))
            .map_err(|e| StepError(format!("Could not read the computer's Kerberos keys: {e}")))?;
        r.log_command("klist -k /etc/krb5.keytab", &klist);
        let principal = keytab_principal(&klist.stdout)
            .ok_or("The computer's Kerberos key file has no entries. The join did not complete properly.")?;
        let kinit = ctx
            .runner
            .run(&CommandSpec::new(
                "kinit",
                &["-k", "-t", "/etc/krb5.keytab", "-c", "MEMORY:rabita-verify", &principal],
            ))
            .map_err(|e| StepError(format!("Could not run kinit: {e}")))?;
        r.log_command(&format!("kinit -k -t /etc/krb5.keytab -c MEMORY:rabita-verify {principal}"), &kinit);
        if !kinit.success() {
            return Err(StepError(format!(
                "The computer could not get a Kerberos ticket: {}",
                last_lines(&kinit.combined(), 2)
            )));
        }
        notes.push("Kerberos ticket OK".to_owned());
        if let Some(user) = req.verify_user.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
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
                std::thread::sleep(Duration::from_secs(if ctx.network { 2 } else { 0 }));
            }
            if !found {
                return Err(StepError(format!(
                    "Joined, but the user '{user}' could not be looked up yet. Wait a minute and check Status; if it still fails, check the user name."
                )));
            }
            notes.push(format!("user '{user}' found"));
        }
        Ok((StepState::Done, notes.join("; "), ()))
    })?;
    Ok(())
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
    use crate::types::JoinOptions;

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
            network: false,
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
        assert!(conf.contains("default_domain_suffix = lab.quietbyte.test"));

        // PAM and GDM steps planned (no display manager detected -> display skipped).
        let lines = fake.call_lines();
        assert!(lines
            .iter()
            .any(|l| l.starts_with("pam-auth-update --package --enable sss --enable")));
        assert!(lines.iter().any(|l| l == "systemctl restart sssd"));
        assert!(lines
            .iter()
            .any(|l| l
                .starts_with("kinit -k -t /etc/krb5.keytab -c MEMORY:rabita-verify LAB-PC-01$@")));

        // State saved for leave.
        let state = Store::new(paths.clone()).load().unwrap();
        assert!(state.joined);
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
            network: false,
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
            network: false,
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
            network: false,
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
            network: false,
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
            network: false,
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
        let out = configure_sssd_text(realmd_sssd_conf(), "lab.quietbyte.test", false);
        assert!(out.contains("[sssd]\ndomains = lab.quietbyte.test\nconfig_file_version = 2\nservices = nss, pam\ndefault_domain_suffix = lab.quietbyte.test\n"));
        assert!(out.contains("cache_credentials = False"));
        assert!(out.contains("ad_gpo_access_control = permissive"));
        assert_eq!(out.matches("use_fully_qualified_names").count(), 1);
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
