// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! The wizard's link to `rabita-helper` over D-Bus.
//!
//! Commands pass the helper's JSON straight through to the web page, which
//! has matching TypeScript types. While a job runs, the helper's signals are
//! forwarded to the page as `rabita:progress`, `rabita:log` and
//! `rabita:finished` events.
//!
//! Set `RABITA_BUS=session` to talk to a development helper started with
//! `rabita-helper --session`.

// The zbus proxy macro and Tauri command macros generate undocumented items,
// and Tauri requires `State`/`AppHandle` by value.
#![allow(missing_docs, clippy::needless_pass_by_value)]

use std::pin::Pin;

use futures_util::{Stream, StreamExt};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::OnceCell;
use zbus::{proxy, Connection, DBusError};
use zeroize::Zeroizing;

/// Client side of `org.quietbyte.Rabita1` (see docs/dbus-api.md).
#[proxy(
    interface = "org.quietbyte.Rabita1",
    default_service = "org.quietbyte.Rabita1",
    default_path = "/org/quietbyte/Rabita1"
)]
trait Rabita {
    fn detect_system(&self) -> zbus::Result<String>;
    fn discover_domain(&self, domain: &str) -> zbus::Result<String>;
    fn status(&self) -> zbus::Result<String>;
    fn join(&self, request: &str, password: &str) -> zbus::Result<u32>;
    fn apply_options(&self, options: &str) -> zbus::Result<u32>;
    #[zbus(property)]
    fn version(&self) -> zbus::Result<String>;
    #[zbus(signal)]
    fn progress(&self, job: u32, step: &str, state: &str, message: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    fn log_line(&self, job: u32, line: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    fn job_finished(&self, job: u32, ok: bool, result_json: &str) -> zbus::Result<()>;
}

/// The bus connection, opened on first use.
#[derive(Default)]
pub struct Bridge {
    conn: OnceCell<Connection>,
}

fn use_session_bus() -> bool {
    std::env::var("RABITA_BUS").is_ok_and(|v| v == "session")
}

impl Bridge {
    async fn proxy(&self) -> Result<RabitaProxy<'static>, String> {
        let conn = self
            .conn
            .get_or_try_init(|| async {
                if use_session_bus() {
                    Connection::session().await
                } else {
                    Connection::system().await
                }
            })
            .await
            .map_err(|e| format!("Rabita Setup could not reach the system message bus: {e}"))?;
        RabitaProxy::new(conn).await.map_err(|e| friendly(&e))
    }
}

const HELPER_MISSING: &str =
    "The Rabita helper service is not installed or could not start. Reinstall Rabita, then try again.";

/// Turns a D-Bus error name and message into words for the wizard.
#[must_use]
pub fn friendly_method_error(name: &str, message: Option<&str>) -> String {
    if name == "org.freedesktop.DBus.Error.ServiceUnknown"
        || name == "org.freedesktop.DBus.Error.NameHasNoOwner"
        || name.starts_with("org.freedesktop.DBus.Error.Spawn.")
    {
        return HELPER_MISSING.to_owned();
    }
    match message.map(str::trim).filter(|m| !m.is_empty()) {
        Some(m) => m.to_owned(),
        None => format!("The Rabita helper reported an error ({name})."),
    }
}

/// Turns any zbus error into words for the wizard.
#[must_use]
pub fn friendly(e: &zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(name, message, _) => {
            friendly_method_error(name.as_str(), message.as_deref())
        }
        zbus::Error::FDO(fdo) => friendly_method_error(fdo.name().as_str(), fdo.description()),
        other => format!("Rabita Setup could not talk to its helper: {other}"),
    }
}

/// Whether this process runs with root's effective user id.
#[must_use]
pub fn running_as_root() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| effective_uid(&s))
        == Some(0)
}

/// The effective uid from `/proc/self/status` text (`Uid: real effective ...`).
#[must_use]
pub fn effective_uid(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|l| l.strip_prefix("Uid:"))
        .and_then(|rest| rest.split_whitespace().nth(1))
        .and_then(|uid| uid.parse().ok())
}

/// A step changed state.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProgressPayload {
    pub job: u32,
    pub step: String,
    pub state: String,
    pub message: String,
}

/// A line for the "Show details" log.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LogPayload {
    pub job: u32,
    pub line: String,
}

/// A job ended; `result_json` is the helper's `JobResult`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FinishedPayload {
    pub job: u32,
    pub ok: bool,
    pub result_json: String,
}

enum Event {
    Progress(ProgressPayload),
    Log(LogPayload),
    Finished(FinishedPayload),
}

type EventStream = Pin<Box<dyn Stream<Item = Event> + Send>>;

/// Subscribes to the helper's job signals. Done before starting a job so no
/// early step is missed.
async fn subscribe(proxy: &RabitaProxy<'static>) -> Result<EventStream, String> {
    // Reading a property makes sure the helper is running, so the
    // subscription follows the right sender.
    let _ = proxy.version().await;
    let progress = proxy
        .receive_progress()
        .await
        .map_err(|e| friendly(&e))?
        .filter_map(|s| async move {
            let a = s.args().ok()?;
            Some(Event::Progress(ProgressPayload {
                job: a.job,
                step: a.step.to_owned(),
                state: a.state.to_owned(),
                message: a.message.to_owned(),
            }))
        });
    let log = proxy
        .receive_log_line()
        .await
        .map_err(|e| friendly(&e))?
        .filter_map(|s| async move {
            let a = s.args().ok()?;
            Some(Event::Log(LogPayload {
                job: a.job,
                line: a.line.to_owned(),
            }))
        });
    let finished = proxy
        .receive_job_finished()
        .await
        .map_err(|e| friendly(&e))?
        .filter_map(|s| async move {
            let a = s.args().ok()?;
            Some(Event::Finished(FinishedPayload {
                job: a.job,
                ok: a.ok,
                result_json: a.result_json.to_owned(),
            }))
        });
    Ok(Box::pin(futures_util::stream::select_all(vec![
        progress.boxed(),
        log.boxed(),
        finished.boxed(),
    ])))
}

/// Forwards one job's signals to the window until it finishes.
async fn forward(app: AppHandle, mut events: EventStream, job: u32) {
    while let Some(event) = events.next().await {
        let sent = match event {
            Event::Progress(p) if p.job == job => app.emit("rabita:progress", p),
            Event::Log(l) if l.job == job => app.emit("rabita:log", l),
            Event::Finished(f) if f.job == job => {
                let _ = app.emit("rabita:finished", f);
                break;
            }
            _ => Ok(()),
        };
        if sent.is_err() {
            break;
        }
    }
}

/// What kind of computer this is (`SystemInfo` JSON).
#[tauri::command]
pub async fn detect_system(bridge: State<'_, Bridge>) -> Result<String, String> {
    bridge
        .proxy()
        .await?
        .detect_system()
        .await
        .map_err(|e| friendly(&e))
}

/// Finds the domain controllers for a domain (`DiscoveryResult` JSON).
#[tauri::command]
pub async fn discover_domain(bridge: State<'_, Bridge>, domain: String) -> Result<String, String> {
    bridge
        .proxy()
        .await?
        .discover_domain(&domain)
        .await
        .map_err(|e| friendly(&e))
}

/// Connection and health report (`StatusReport` JSON).
#[tauri::command]
pub async fn status(bridge: State<'_, Bridge>) -> Result<String, String> {
    bridge
        .proxy()
        .await?
        .status()
        .await
        .map_err(|e| friendly(&e))
}

/// Starts a join and returns its job id. The admin password is passed on
/// once and wiped from this process's memory straight after.
#[tauri::command]
pub async fn join(
    app: AppHandle,
    bridge: State<'_, Bridge>,
    request: String,
    password: String,
) -> Result<u32, String> {
    let password = Zeroizing::new(password);
    let proxy = bridge.proxy().await?;
    let events = subscribe(&proxy).await?;
    let job = proxy
        .join(&request, &password)
        .await
        .map_err(|e| friendly(&e))?;
    drop(password);
    tauri::async_runtime::spawn(forward(app, events, job));
    Ok(job)
}

/// Re-applies options, or finishes a setup that stopped after joining.
#[tauri::command]
pub async fn apply_options(
    app: AppHandle,
    bridge: State<'_, Bridge>,
    options: String,
) -> Result<u32, String> {
    let proxy = bridge.proxy().await?;
    let events = subscribe(&proxy).await?;
    let job = proxy
        .apply_options(&options)
        .await
        .map_err(|e| friendly(&e))?;
    tauri::async_runtime::spawn(forward(app, events, job));
    Ok(job)
}

/// Closes the wizard.
#[tauri::command]
pub fn quit(app: AppHandle) {
    app.exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_helper_is_explained() {
        for name in [
            "org.freedesktop.DBus.Error.ServiceUnknown",
            "org.freedesktop.DBus.Error.NameHasNoOwner",
            "org.freedesktop.DBus.Error.Spawn.ChildExited",
        ] {
            assert_eq!(
                friendly_method_error(name, Some("raw text")),
                HELPER_MISSING
            );
        }
    }

    #[test]
    fn helper_messages_pass_through() {
        assert_eq!(
            friendly_method_error(
                "org.freedesktop.DBus.Error.AccessDenied",
                Some("Authentication was cancelled.")
            ),
            "Authentication was cancelled."
        );
        assert_eq!(
            friendly_method_error("org.example.Odd", Some("  ")),
            "The Rabita helper reported an error (org.example.Odd)."
        );
    }

    #[test]
    fn reads_the_effective_uid() {
        let status = "Name:\trabita-app\nUid:\t1000\t0\t1000\t1000\nGid:\t1000\t1000\t1000\t1000\n";
        assert_eq!(effective_uid(status), Some(0));
        assert_eq!(effective_uid("Uid:\t1000\t1000\t1000\t1000\n"), Some(1000));
        assert_eq!(effective_uid("Name: x\n"), None);
    }
}
