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
                    network: true,
                };
                work(&ctx)
            })
            .await
            .unwrap_or_else(|e| JobResult {
                ok: false,
                summary: format!("The setup job crashed: {e}"),
                steps: Vec::new(),
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

/// Re-applies the home-folder and login-screen options.
#[must_use]
pub fn apply_options_job(ctx: &JobContext<'_>, opts: &JoinOptions) -> JobResult {
    use crate::backup::Store;
    use crate::progress::{Collector, Reporter, Tee};
    use rabita_backends::SystemFacts;
    use std::time::Duration;

    let collector = Collector::new();
    let tee = Tee(vec![ctx.sink, &collector]);
    let r = Reporter::new(&tee);
    let store = Store::new(ctx.paths.clone());
    let mut state = store.load().unwrap_or_default();
    let info = system::detect(ctx.runner, ctx.paths);
    let backend = info.family.map(rabita_backends::DistroFamily::backend);
    let facts = SystemFacts {
        mkhomedir_already_enabled: state.mkhomedir_was_enabled,
        ..system::gather_facts(ctx.paths)
    };
    let joined = state.joined;

    let mut apply = |id: &str, label: &str, actions: Vec<rabita_backends::Action>| {
        r.step(id, label, StepState::Running, "");
        if actions.is_empty() {
            r.step(id, label, StepState::Skipped, "No change needed.");
            return Ok(());
        }
        match join::execute_actions(
            ctx,
            &store,
            &mut state,
            &r,
            &actions,
            Duration::from_secs(120),
        ) {
            Ok(()) => {
                r.step(id, label, StepState::Done, "Done.");
                Ok(())
            }
            Err(e) => {
                r.step(id, label, StepState::Failed, &e.0);
                Err(e)
            }
        }
    };

    let outcome: Result<(), join::StepError> = match &backend {
        Some(b) if joined => (|| {
            let access = join::login_access_actions(&opts.login_access).inspect_err(|e| {
                r.step("access", "Choose who can sign in", StepState::Failed, &e.0);
            })?;
            apply("access", "Choose who can sign in", access)?;
            apply(
                "home",
                "Create home folders on first login",
                if opts.create_home {
                    b.enable_login_integration(&facts)
                } else {
                    b.disable_login_integration(&facts)
                },
            )?;
            apply(
                "display",
                "Show a username box on the login screen",
                if opts.manual_login {
                    b.enable_manual_login(&facts)
                } else {
                    b.disable_manual_login(&facts)
                },
            )
        })(),
        _ => Err(join::StepError::from(
            "This computer is not connected to a domain yet.",
        )),
    };
    drop(apply);
    let (mut ok, mut failure) = match outcome {
        Ok(()) => (true, String::new()),
        Err(e) => (false, e.0),
    };
    if ok {
        state.options = opts.clone();
        state.options.login_access = opts.login_access.normalised();
        if let Err(e) = store.save(&state) {
            ok = false;
            failure = format!("Could not save Rabita's state: {e}");
        }
    }
    JobResult {
        ok,
        summary: if ok {
            "Options applied.".to_owned()
        } else {
            failure
        },
        steps: collector.outcomes(),
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
        let paths = Paths::under(dir);
        let p = paths.os_release();
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "ID=ubuntu\nID_LIKE=debian\nPRETTY_NAME=Ubuntu\n").unwrap();
        Store::new(paths.clone())
            .save(&State {
                domain: Some("lab.test".into()),
                joined: true,
                mkhomedir_was_enabled: true,
                ..State::default()
            })
            .unwrap();
        paths
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
            network: false,
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
            network: false,
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
            network: false,
        };
        let result = apply_options_job(&ctx2, &JoinOptions::default());
        assert!(!result.ok);
        assert!(result.summary.contains("not connected"));
        let _ = Arc::new(());
    }
}
