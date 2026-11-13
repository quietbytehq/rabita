// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! The `org.quietbyte.Rabita1` D-Bus interface.
//!
//! Long jobs (`Join`, `Leave`, `ApplyOptions`) return a job id right away and
//! report through signals, so the wizard's checklist updates live and no
//! D-Bus call has to wait minutes for `apt-get`:
//!
//! - `Progress(job, step, state, message)`
//! - `LogLine(job, line)`
//! - `JobFinished(job, ok, result_json)`
//!
//! Only one job runs at a time. Structured data travels as JSON strings (see
//! [`crate::types`]).

// zbus macros generate items without doc comments.
#![allow(missing_docs)]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use tokio::sync::mpsc;
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::{fdo, interface, Connection};
use zeroize::Zeroizing;

use crate::cmd::{Runner, SystemRunner};
use crate::join::{self, JobContext};
use crate::paths::Paths;
use crate::polkit::{self, Bus};
use crate::progress::{FnSink, ProgressEvent};
use crate::types::{JobResult, JoinOptions, JoinRequest, LeaveRequest, StepState};
use crate::{discover, leave, status, system, OBJECT_PATH, VERSION};

/// Shared state behind the interface.
pub struct Inner {
    runner: Arc<dyn Runner>,
    paths: Paths,
    bus: Bus,
    busy: AtomicBool,
    next_job: AtomicU32,
}

/// The D-Bus object.
#[derive(Clone)]
pub struct RabitaService {
    inner: Arc<Inner>,
}

impl RabitaService {
    /// Creates the service for the real system.
    #[must_use]
    pub fn new(bus: Bus) -> Self {
        Self::with(Arc::new(SystemRunner), Paths::system(), bus)
    }

    /// Creates the service with a custom runner and layout (tests).
    #[must_use]
    pub fn with(runner: Arc<dyn Runner>, paths: Paths, bus: Bus) -> Self {
        Self {
            inner: Arc::new(Inner {
                runner,
                paths,
                bus,
                busy: AtomicBool::new(false),
                next_job: AtomicU32::new(1),
            }),
        }
    }

    fn to_json<T: serde::Serialize>(value: &T) -> fdo::Result<String> {
        serde_json::to_string(value).map_err(|e| fdo::Error::Failed(format!("serialisation: {e}")))
    }

    fn from_json<T: serde::de::DeserializeOwned>(json: &str) -> fdo::Result<T> {
        serde_json::from_str(json).map_err(|e| fdo::Error::InvalidArgs(format!("bad request: {e}")))
    }

    /// Starts a background job and wires its progress to signals.
    fn start_job(
        &self,
        conn: &Connection,
        work: impl FnOnce(&JobContext<'_>) -> JobResult + Send + 'static,
    ) -> fdo::Result<u32> {
        if self
            .inner
            .busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(fdo::Error::Failed(
                "Another setup job is still running. Wait for it to finish.".into(),
            ));
        }
        let job = self.inner.next_job.fetch_add(1, Ordering::SeqCst);
        let (tx, mut rx) = mpsc::unbounded_channel::<ProgressEvent>();
        let inner = Arc::clone(&self.inner);
        let conn = conn.clone();

        // Forward progress events to D-Bus signals.
        let emitter_conn = conn.clone();
        let forwarder = tokio::spawn(async move {
            let Ok(emitter) = SignalEmitter::new(&emitter_conn, OBJECT_PATH) else {
                return;
            };
            while let Some(ev) = rx.recv().await {
                let _ = match ev {
                    ProgressEvent::Step {
                        id, state, message, ..
                    } => Self::progress(&emitter, job, &id, state.as_str(), &message).await,
                    ProgressEvent::Log(line) => Self::log_line(&emitter, job, &line).await,
                };
            }
        });

        tokio::spawn(async move {
            let inner_for_work = Arc::clone(&inner);
            let result = tokio::task::spawn_blocking(move || {
                let sink = FnSink(move |ev| {
                    let _ = tx.send(ev);
                });
                let ctx = JobContext {
                    runner: inner_for_work.runner.as_ref(),
                    paths: &inner_for_work.paths,
                    sink: &sink,
                    network: Some(&crate::discover::LiveNetwork),
                };
                work(&ctx)
            })
            .await
            .unwrap_or_else(|e| JobResult {
                ok: false,
                summary: format!("The setup job crashed: {e}"),
                ..JobResult::default()
            });
            // Let the forwarder drain remaining events before JobFinished.
            let _ = forwarder.await;
            inner.busy.store(false, Ordering::SeqCst);
            if let Ok(emitter) = SignalEmitter::new(&conn, OBJECT_PATH) {
                let json = serde_json::to_string(&result).unwrap_or_default();
                let _ = Self::job_finished(&emitter, job, result.ok, &json).await;
            }
        });
        Ok(job)
    }
}

#[interface(name = "org.quietbyte.Rabita1")]
impl RabitaService {
    /// What kind of computer this is (JSON `SystemInfo`). No authorisation
    /// needed.
    async fn detect_system(&self) -> fdo::Result<String> {
        let inner = Arc::clone(&self.inner);
        let info = tokio::task::spawn_blocking(move || {
            system::detect(inner.runner.as_ref(), &inner.paths)
        })
        .await
        .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        Self::to_json(&info)
    }

    /// Finds the domain controllers for `domain` (JSON `DiscoveryResult`).
    /// No authorisation needed.
    async fn discover_domain(&self, domain: String) -> fdo::Result<String> {
        let inner = Arc::clone(&self.inner);
        let result = tokio::task::spawn_blocking(move || {
            discover::discover(&domain, inner.runner.as_ref(), &inner.paths, true)
        })
        .await
        .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        Self::to_json(&result)
    }

    /// Connection and health report (JSON `StatusReport`). No authorisation
    /// needed.
    async fn status(&self) -> fdo::Result<String> {
        let inner = Arc::clone(&self.inner);
        let report = tokio::task::spawn_blocking(move || {
            status::report(inner.runner.as_ref(), &inner.paths, true)
        })
        .await
        .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        Self::to_json(&report)
    }

    /// Joins the domain. `request` is JSON `JoinRequest`; `password` is the
    /// admin password, used once and wiped. Returns a job id; watch the
    /// `Progress` and `JobFinished` signals. Requires polkit authorisation.
    async fn join(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        request: String,
        password: String,
    ) -> fdo::Result<u32> {
        let password = Zeroizing::new(password);
        polkit::require_manage(conn, self.inner.bus, &header).await?;
        let req: JoinRequest = Self::from_json(&request)?;
        self.start_job(conn, move |ctx| join::run_join(ctx, &req, password))
    }

    /// Leaves the domain and restores every backed-up file. `request` is
    /// JSON `LeaveRequest`; `password` is only needed when removing the
    /// computer account (pass an empty string otherwise). Requires polkit
    /// authorisation.
    async fn leave(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        request: String,
        password: String,
    ) -> fdo::Result<u32> {
        let password = Zeroizing::new(password);
        polkit::require_manage(conn, self.inner.bus, &header).await?;
        let req: LeaveRequest = Self::from_json(&request)?;
        self.start_job(conn, move |ctx| leave::run_leave(ctx, &req, password))
    }

    /// Re-applies the wizard options (JSON `JoinOptions`) on an already
    /// joined computer. Requires polkit authorisation.
    async fn apply_options(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        options: String,
    ) -> fdo::Result<u32> {
        polkit::require_manage(conn, self.inner.bus, &header).await?;
        let opts: JoinOptions = Self::from_json(&options)?;
        self.start_job(conn, move |ctx| apply_options_job(ctx, &opts))
    }

    /// Helper version.
    #[zbus(property)]
    #[allow(clippy::unused_self)] // zbus properties take &self
    fn version(&self) -> String {
        VERSION.to_owned()
    }

    /// Whether a job is running.
    #[zbus(property)]
    fn busy(&self) -> bool {
        self.inner.busy.load(Ordering::SeqCst)
    }

    /// A step of a job changed state. `state` is one of `pending`,
    /// `running`, `done`, `failed`, `skipped`.
    #[zbus(signal)]
    async fn progress(
        emitter: &SignalEmitter<'_>,
        job: u32,
        step: &str,
        state: &str,
        message: &str,
    ) -> zbus::Result<()>;

    /// A line for the technical log.
    #[zbus(signal)]
    async fn log_line(emitter: &SignalEmitter<'_>, job: u32, line: &str) -> zbus::Result<()>;

    /// A job ended. `result_json` is a `JobResult`.
    #[zbus(signal)]
    async fn job_finished(
        emitter: &SignalEmitter<'_>,
        job: u32,
        ok: bool,
        result_json: &str,
    ) -> zbus::Result<()>;
}

/// Re-applies the wizard options on a joined computer, or finishes an
/// interrupted setup.
///
/// When the computer is "joined but not finished" (a step after the join
/// failed), or was joined by an older Rabita, or its `sssd.conf` still has a
/// setting that stops SSSD 2.10+ from starting, every post-join step runs:
/// sign-in setup, who can sign in, home folders, login screen and the final
/// check. Otherwise only the option steps run.
#[must_use]
pub fn apply_options_job(ctx: &JobContext<'_>, opts: &JoinOptions) -> JobResult {
    use crate::backup::Store;
    use crate::progress::{Collector, Reporter, Tee};

    let collector = Collector::new();
    let tee = Tee(vec![ctx.sink, &collector]);
    let r = Reporter::new(&tee);
    let steps = join::Steps::new(Reporter::new(&tee));
    let store = Store::new(ctx.paths.clone());
    let mut state = store.load().unwrap_or_default();
    let info = system::detect(ctx.runner, ctx.paths);
    let backend = match info.family.map(rabita_backends::DistroFamily::backend) {
        Some(b) if state.joined => b,
        _ => {
            return JobResult {
                summary: "This computer is not connected to a domain yet.".to_owned(),
                steps: collector.outcomes(),
                ..JobResult::default()
            }
        }
    };
    // Reject a bad choice before anything changes.
    if let Err(e) = join::login_access_actions(&opts.login_access) {
        r.step(
            "access",
            join::step_label("access"),
            StepState::Failed,
            &e.0,
        );
        return JobResult {
            summary: e.0,
            steps: collector.outcomes(),
            ..JobResult::default()
        };
    }

    let mut options = opts.clone();
    options.login_access = opts.login_access.normalised();
    let domain = state.domain.clone().unwrap_or_default();
    let sssd_text = std::fs::read_to_string(ctx.paths.sssd_conf()).unwrap_or_default();
    let finishing = state.setup_finished != Some(true) || join::sssd_conf_needs_repair(&sssd_text);
    let step_ids: &[&str] = if finishing {
        &join::POST_JOIN_STEPS
    } else {
        &["access", "home", "display"]
    };
    for id in step_ids {
        r.step(id, join::step_label(id), StepState::Pending, "");
    }
    let verify_user = state.verify_user.clone();
    let preferred_dc = state.domain_controller.clone();
    let pj = join::PostJoin {
        domain: &domain,
        options: &options,
        verify_user: verify_user.as_deref(),
        preferred_dc: preferred_dc.as_deref(),
        steps: step_ids,
        undo_disabled: true,
        track_unfinished: finishing,
    };
    let mut outcome = join::run_post_join(
        ctx,
        &store,
        &mut state,
        backend.as_ref(),
        info.display_manager,
        &pj,
        &steps,
    )
    .map_err(|e| e.0);
    if outcome.is_ok() {
        state.options = options;
        if let Err(e) = store.save(&state) {
            outcome = Err(format!("Could not save Rabita's state: {e}"));
        }
    }
    let unfinished = finishing && outcome.is_err() && state.setup_finished == Some(false);
    let mut steps_out = collector.outcomes();
    if outcome.is_err() {
        join::mark_not_run(&mut steps_out, unfinished);
    }
    JobResult {
        ok: outcome.is_ok(),
        summary: match &outcome {
            Ok(()) if finishing => {
                format!("Setup finished. This computer is fully connected to {domain}.")
            }
            Ok(()) => "Options applied.".to_owned(),
            Err(e) if unfinished => join::not_finished_summary(&domain, e, &state.remaining_steps),
            Err(e) => e.clone(),
        },
        steps: steps_out,
        joined_not_finished: unfinished,
        remaining_steps: if unfinished {
            state.remaining_steps.clone()
        } else {
            Vec::new()
        },
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::sync::Arc;

    use super::apply_options_job;
    use crate::backup::{State, Store};
    use crate::cmd::FakeRunner;
    use crate::join::JobContext;
    use crate::paths::Paths;
    use crate::progress::Collector;
    use crate::types::{JoinOptions, LoginAccess, StepState};

    fn joined_lab(dir: &Path) -> Paths {
        lab_with(dir, Some(true))
    }

    fn lab_with(dir: &Path, setup_finished: Option<bool>) -> Paths {
        let paths = Paths::under(dir);
        let p = paths.os_release();
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "ID=ubuntu\nID_LIKE=debian\nPRETTY_NAME=Ubuntu\n").unwrap();
        Store::new(paths.clone())
            .save(&State {
                domain: Some("lab.test".into()),
                joined: true,
                mkhomedir_was_enabled: true,
                setup_finished,
                ..State::default()
            })
            .unwrap();
        paths
    }

    const KLIST: &str = "   2 PC1$@LAB.TEST\n";

    #[test]
    fn apply_on_a_finished_computer_runs_only_the_option_steps() {
        let dir = tempfile::tempdir().unwrap();
        let paths = joined_lab(dir.path());
        let fake = FakeRunner::new();
        let collector = Collector::new();
        let ctx = JobContext {
            runner: &fake,
            paths: &paths,
            sink: &collector,
            network: None,
        };
        let result = apply_options_job(&ctx, &JoinOptions::default());
        assert!(result.ok, "{}", result.summary);
        assert_eq!(result.summary, "Options applied.");
        let ids: Vec<&str> = result.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["access", "home", "display"]);
        let lines = fake.call_lines();
        assert!(!lines
            .iter()
            .any(|l| l.starts_with("sssctl") || l == "systemctl restart sssd"));
    }

    /// Computers joined by an older Rabita have no setup record and may
    /// carry the sssd.conf setting that stops SSSD 2.10+; Finish setup
    /// brings them up to date.
    #[test]
    fn apply_finishes_and_repairs_computers_joined_by_an_older_rabita() {
        for setup_finished in [None, Some(true)] {
            let dir = tempfile::tempdir().unwrap();
            let paths = lab_with(dir.path(), setup_finished);
            let conf = paths.sssd_conf();
            fs::create_dir_all(conf.parent().unwrap()).unwrap();
            fs::write(
                &conf,
                "[sssd]\nconfig_file_version = 2\ndefault_domain_suffix = lab.test\n\n[domain/lab.test]\nuse_fully_qualified_names = False\n",
            )
            .unwrap();
            let fake = FakeRunner::new();
            fake.ok("klist", KLIST);
            let collector = Collector::new();
            let ctx = JobContext {
                runner: &fake,
                paths: &paths,
                sink: &collector,
                network: None,
            };
            let result = apply_options_job(&ctx, &JoinOptions::default());
            assert!(result.ok, "{setup_finished:?}: {}", result.summary);
            assert_eq!(
                result.summary,
                "Setup finished. This computer is fully connected to lab.test."
            );
            let text = fs::read_to_string(&conf).unwrap();
            assert!(
                !text.contains("default_domain_suffix") && !text.contains("config_file_version"),
                "{text}"
            );
            assert_eq!(Store::new(paths).load().unwrap().setup_finished, Some(true));
        }
    }

    #[test]
    fn a_failed_finish_stays_joined_but_not_finished() {
        let dir = tempfile::tempdir().unwrap();
        let paths = lab_with(dir.path(), Some(false));
        let fake = FakeRunner::new();
        fake.fail("systemctl restart", "Job for sssd.service failed.");
        let collector = Collector::new();
        let ctx = JobContext {
            runner: &fake,
            paths: &paths,
            sink: &collector,
            network: None,
        };
        let result = apply_options_job(&ctx, &JoinOptions::default());
        assert!(!result.ok && result.joined_not_finished);
        assert!(result.summary.starts_with(
            "This computer is joined to lab.test but not finished. The sign-in service (SSSD) did not start"
        ), "{}", result.summary);
        assert!(fake
            .call_lines()
            .iter()
            .any(|l| l.starts_with("journalctl -u sssd")));
        let state = Store::new(paths).load().unwrap();
        assert_eq!(state.setup_finished, Some(false));
        assert_eq!(state.remaining_steps, crate::join::POST_JOIN_STEPS);
        let _ = StepState::Done;
    }

    #[test]
    fn apply_options_restricts_and_reopens_login() {
        let dir = tempfile::tempdir().unwrap();
        let paths = joined_lab(dir.path());
        let fake = FakeRunner::new();
        fake.ok("realm list", "lab.test\n  configured: kerberos-member\n");
        let collector = Collector::new();
        let ctx = JobContext {
            runner: &fake,
            paths: &paths,
            sink: &collector,
            network: None,
        };
        let opts = JoinOptions {
            login_access: LoginAccess::Groups {
                groups: vec!["Linux Users".into()],
            },
            ..JoinOptions::default()
        };
        let result = apply_options_job(&ctx, &opts);
        assert!(result.ok, "{}", result.summary);
        let lines = fake.call_lines();
        assert!(lines.contains(&"realm deny --all".to_owned()));
        assert!(lines.contains(&"realm permit -g Linux Users".to_owned()));
        assert_eq!(
            Store::new(paths.clone())
                .load()
                .unwrap()
                .options
                .login_access,
            opts.login_access
        );
        let access = result.steps.iter().find(|s| s.id == "access").unwrap();
        assert_eq!(access.state, StepState::Done);

        let result = apply_options_job(&ctx, &JoinOptions::default());
        assert!(result.ok, "{}", result.summary);
        assert!(fake.call_lines().contains(&"realm permit --all".to_owned()));
        assert_eq!(
            Store::new(paths).load().unwrap().options.login_access,
            LoginAccess::All
        );
    }

    #[test]
    fn apply_options_rejects_empty_groups_and_unjoined_machines() {
        let dir = tempfile::tempdir().unwrap();
        let paths = joined_lab(dir.path());
        let fake = FakeRunner::new();
        let collector = Collector::new();
        let ctx = JobContext {
            runner: &fake,
            paths: &paths,
            sink: &collector,
            network: None,
        };
        let opts = JoinOptions {
            login_access: LoginAccess::Groups { groups: vec![] },
            ..JoinOptions::default()
        };
        let result = apply_options_job(&ctx, &opts);
        assert!(!result.ok);
        assert!(result.summary.starts_with("Enter at least one group"));
        assert!(!fake.call_lines().iter().any(|l| l.starts_with("realm")));

        let dir2 = tempfile::tempdir().unwrap();
        let paths2 = Paths::under(dir2.path());
        let ctx2 = JobContext {
            runner: &fake,
            paths: &paths2,
            sink: &collector,
            network: None,
        };
        let result = apply_options_job(&ctx2, &JoinOptions::default());
        assert!(!result.ok);
        assert!(result.summary.contains("not connected"));
        let _ = Arc::new(());
    }
}
