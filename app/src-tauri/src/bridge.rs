// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! The wizard's link to `rabita-helper` over D-Bus.
//!
//! Commands pass the helper's JSON straight through to the web page, which
//! has matching TypeScript types. While a job runs, the helper's signals are
//! forwarded to the page as `rabita:progress`, `rabita:log` and
//! `rabita:finished` events.
//!
//! Every call has a time limit, so the window never waits forever. The first
//! contact also checks that the helper that answered is the same version as
//! the window (a leftover test install would not be). While a job runs, a
//! watchdog checks every few seconds that the helper is still there and
//! still working on it; if not, the window gets a "lost contact" result.
//! When the helper is missing, stuck or the wrong copy, the error says why
//! (see `diagnose`).
//!
//! Set `RABITA_BUS=session` to talk to a development helper started with
//! `rabita-helper --session`. With it, `RABITA_TIMEOUT_SECS` shortens the
//! time limits for testing.

// The zbus proxy macro and Tauri command macros generate undocumented items,
// and Tauri requires `State`/`AppHandle` by value.
#![allow(missing_docs, clippy::needless_pass_by_value)]

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use futures_util::{Stream, StreamExt};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::OnceCell;
use tokio::time::timeout;
use zbus::names::{BusName, OwnedUniqueName, WellKnownName};
use zbus::proxy::CacheProperties;
use zbus::{proxy, Connection, DBusError};
use zeroize::Zeroizing;

use crate::diagnose;

/// The helper's well-known bus name.
const BUS_NAME: &str = "org.quietbyte.Rabita1";
/// This window's version; the helper must match it.
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// First contact, including D-Bus starting the helper.
pub const START_LIMIT: Duration = Duration::from_secs(30);
/// Reading this computer's facts.
pub const QUICK_LIMIT: Duration = Duration::from_secs(45);
/// Finding domain controllers, health checks, looking for DNS servers.
pub const SLOW_LIMIT: Duration = Duration::from_secs(120);
/// Privileged calls wait for the desktop's password window. The helper
/// closes that window after 5 minutes, so this is a little longer.
pub const PASSWORD_LIMIT: Duration = Duration::from_secs(6 * 60);
/// How often a running job's helper is checked on.
const WATCH_EVERY: Duration = Duration::from_secs(15);
/// Limit for one watchdog question.
const WATCH_LIMIT: Duration = Duration::from_secs(10);

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
    fn leave(&self, request: &str, password: &str) -> zbus::Result<u32>;
    fn find_dns_servers(&self, domain: &str) -> zbus::Result<String>;
    fn set_dns_server(&self, domain: &str, server: &str) -> zbus::Result<String>;
    fn restore_dns(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn version(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn busy(&self) -> zbus::Result<bool>;
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

/// `base`, or the development override `RABITA_TIMEOUT_SECS` (session bus
/// only; password waits get twice that).
fn limit(base: Duration) -> Duration {
    let dev = use_session_bus()
        .then(|| std::env::var("RABITA_TIMEOUT_SECS").ok())
        .flatten()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&n| n > 0);
    match dev {
        Some(n) if base == PASSWORD_LIMIT => Duration::from_secs(n * 2),
        Some(n) => Duration::from_secs(n),
        None => base,
    }
}

const fn secs(d: Duration) -> u64 {
    d.as_secs()
}

fn bus_name() -> BusName<'static> {
    BusName::WellKnown(WellKnownName::from_static_str_unchecked(BUS_NAME))
}

impl Bridge {
    async fn connection(&self) -> Result<Connection, String> {
        let open = async {
            if use_session_bus() {
                Connection::session().await
            } else {
                Connection::system().await
            }
        };
        self.conn
            .get_or_try_init(|| async {
                match timeout(limit(START_LIMIT), open).await {
                    Ok(r) => r.map_err(|e| {
                        format!("Rabita Setup could not reach the system message bus: {e}")
                    }),
                    Err(_) => Err(format!(
                        "Rabita Setup could not reach the system message bus within {} seconds. Restart the computer and try again.",
                        secs(limit(START_LIMIT))
                    )),
                }
            })
            .await
            .cloned()
    }

    /// A proxy for a helper that answered and is the right version.
    async fn proxy(&self) -> Result<RabitaProxy<'static>, String> {
        let conn = self.connection().await?;
        // No property cache: `Busy` and `Version` must be read fresh.
        let proxy = RabitaProxy::builder(&conn)
            .cache_properties(CacheProperties::No)
            .build()
            .await
            .map_err(|e| friendly(&e))?;
        // First contact: D-Bus starts the helper here if it isn't running.
        match timeout(limit(START_LIMIT), proxy.version()).await {
            Err(_) => Err(explain_with_reasons(
                &conn,
                &format!(
                    "Rabita Setup's background service did not answer within {} seconds.",
                    secs(limit(START_LIMIT))
                ),
            )
            .await),
            Ok(Err(e)) => Err(explain_error(&conn, &e).await),
            Ok(Ok(v)) if v != APP_VERSION => {
                Err(explain_with_reasons(&conn, &version_mismatch(&v)).await)
            }
            Ok(Ok(_)) => Ok(proxy),
        }
    }
}

fn version_mismatch(helper: &str) -> String {
    format!(
        "This window is Rabita Setup {APP_VERSION}, but the background service that answered is version {helper}, so an older or leftover copy of it is running."
    )
}

/// Runs one helper call with a time limit.
async fn call<T>(
    conn: &Connection,
    max: Duration,
    work: impl Future<Output = zbus::Result<T>>,
) -> Result<T, String> {
    match timeout(max, work).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(explain_error(conn, &e).await),
        Err(_) => Err(explain_with_reasons(
            conn,
            &format!(
                "Rabita Setup's background service stopped answering: it took longer than {} seconds. Nothing more will happen in this window.",
                secs(max)
            ),
        )
        .await),
    }
}

/// The command line of the process that owns the helper's name, if any.
/// Asking the bus about an owner never starts the helper.
async fn owner_cmdline(conn: &Connection) -> Option<String> {
    let dbus = zbus::fdo::DBusProxy::new(conn).await.ok()?;
    let pid = timeout(WATCH_LIMIT, dbus.get_connection_unix_process_id(bus_name()))
        .await
        .ok()?
        .ok()?;
    std::fs::read_to_string(format!("/proc/{pid}/cmdline")).ok()
}

/// `message` plus the likely reasons and what to do.
async fn explain_with_reasons(conn: &Connection, message: &str) -> String {
    let reasons = if use_session_bus() {
        // A development helper on the session bus: the system's files
        // say nothing about it.
        Vec::new()
    } else {
        diagnose::reasons(Path::new("/"), owner_cmdline(conn).await.as_deref())
    };
    diagnose::explain(message, &reasons)
}

/// Words for a failed call; a missing or broken helper gets the reasons too.
async fn explain_error(conn: &Connection, e: &zbus::Error) -> String {
    let text = friendly(e);
    if [HELPER_MISSING, HELPER_WRONG, HELPER_SILENT].contains(&text.as_str()) {
        explain_with_reasons(conn, &text).await
    } else {
        text
    }
}

const HELPER_MISSING: &str =
    "Rabita Setup's background service is not installed or could not start.";
const HELPER_WRONG: &str =
    "Rabita Setup's background service did not understand the request, so an older or different copy of it is running.";
const HELPER_SILENT: &str = "Rabita Setup's background service did not answer.";

/// Turns a D-Bus error name and message into words for the wizard.
#[must_use]
pub fn friendly_method_error(name: &str, message: Option<&str>) -> String {
    if name == "org.freedesktop.DBus.Error.ServiceUnknown"
        || name == "org.freedesktop.DBus.Error.NameHasNoOwner"
        || name.starts_with("org.freedesktop.DBus.Error.Spawn.")
        || name.starts_with("org.freedesktop.systemd1.")
    {
        return HELPER_MISSING.to_owned();
    }
    if name == "org.freedesktop.DBus.Error.UnknownMethod"
        || name == "org.freedesktop.DBus.Error.UnknownObject"
        || name == "org.freedesktop.DBus.Error.UnknownInterface"
        || name == "org.freedesktop.DBus.Error.UnknownProperty"
    {
        return HELPER_WRONG.to_owned();
    }
    if name == "org.freedesktop.DBus.Error.TimedOut" || name == "org.freedesktop.DBus.Error.NoReply"
    {
        return HELPER_SILENT.to_owned();
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
/// early step is missed. `proxy` has already answered, so the helper runs.
async fn subscribe(proxy: &RabitaProxy<'static>) -> Result<EventStream, String> {
    let conn = proxy.inner().connection().clone();
    let progress = call(&conn, limit(QUICK_LIMIT), proxy.receive_progress())
        .await?
        .filter_map(|s| async move {
            let a = s.args().ok()?;
            Some(Event::Progress(ProgressPayload {
                job: a.job,
                step: a.step.to_owned(),
                state: a.state.to_owned(),
                message: a.message.to_owned(),
            }))
        });
    let log = call(&conn, limit(QUICK_LIMIT), proxy.receive_log_line())
        .await?
        .filter_map(|s| async move {
            let a = s.args().ok()?;
            Some(Event::Log(LogPayload {
                job: a.job,
                line: a.line.to_owned(),
            }))
        });
    let finished = call(&conn, limit(QUICK_LIMIT), proxy.receive_job_finished())
        .await?
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

/// The helper's current unique bus name. Never starts the helper.
async fn current_owner(conn: &Connection) -> Result<Option<OwnedUniqueName>, ()> {
    let dbus = zbus::fdo::DBusProxy::new(conn).await.map_err(|_| ())?;
    match timeout(WATCH_LIMIT, dbus.get_name_owner(bus_name())).await {
        Ok(Ok(owner)) => Ok(Some(owner)),
        Ok(Err(_)) => Ok(None),
        Err(_) => Err(()),
    }
}

/// What the watchdog saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// Still working on a job.
    Busy,
    /// Running, but not working on anything.
    Idle,
    /// Gone, or replaced by a new copy (the job's result is lost).
    Gone,
    /// Did not answer in time.
    NoAnswer,
}

async fn check_health(proxy: &RabitaProxy<'static>, owner: Option<&OwnedUniqueName>) -> Health {
    let conn = proxy.inner().connection();
    match current_owner(conn).await {
        Err(()) => return Health::NoAnswer,
        Ok(now) if now.as_ref() != owner => return Health::Gone,
        Ok(_) => {}
    }
    match timeout(WATCH_LIMIT, proxy.busy()).await {
        Ok(Ok(true)) => Health::Busy,
        Ok(Ok(false)) => Health::Idle,
        _ => Health::NoAnswer,
    }
}

/// Counts what the watchdog saw and decides when to give up on a job.
#[derive(Debug, Default)]
pub struct Watchdog {
    idle: u32,
    silent: u32,
}

/// Why a job was given up on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lost {
    Gone,
    NoResult,
    Stuck,
}

impl Watchdog {
    /// Checks in a row with the helper idle before giving up (a finished
    /// job's result may be on its way for one check).
    pub const IDLE_CHECKS: u32 = 2;
    /// Unanswered checks in a row before giving up (about a minute).
    pub const SILENT_CHECKS: u32 = 4;

    /// A job event arrived: the helper is clearly alive.
    pub fn heard(&mut self) {
        self.idle = 0;
        self.silent = 0;
    }

    /// Records one check; `Some` means stop waiting.
    pub fn saw(&mut self, h: Health) -> Option<Lost> {
        match h {
            Health::Busy => {
                self.heard();
                None
            }
            Health::Gone => Some(Lost::Gone),
            Health::Idle => {
                self.idle += 1;
                self.silent = 0;
                (self.idle >= Self::IDLE_CHECKS).then_some(Lost::NoResult)
            }
            Health::NoAnswer => {
                self.silent += 1;
                (self.silent >= Self::SILENT_CHECKS).then_some(Lost::Stuck)
            }
        }
    }
}

/// The `JobResult` the window gets when the helper's own never comes.
#[derive(Debug, Serialize)]
#[allow(clippy::struct_excessive_bools)] // mirrors the helper's JobResult
struct LostResult {
    ok: bool,
    summary: String,
    steps: Vec<()>,
    joined_not_finished: bool,
    remaining_steps: Vec<()>,
    left_locally_only: bool,
    lost_contact: bool,
}

/// Words for a lost job.
#[must_use]
pub fn lost_summary(why: Lost) -> String {
    let what = match why {
        Lost::Gone => "Rabita Setup lost contact with its background service before this finished: the service stopped or was restarted.",
        Lost::NoResult => "Rabita Setup did not receive the result of this job from its background service.",
        Lost::Stuck => "Rabita Setup's background service has not answered for about a minute.",
    };
    format!(
        "{what} The computer may be partly set up. Close Rabita Setup and open it again: it shows what state the computer is in and what can be finished or undone."
    )
}

fn lost_payload(job: u32, why: Lost) -> FinishedPayload {
    let result = LostResult {
        ok: false,
        summary: lost_summary(why),
        steps: Vec::new(),
        joined_not_finished: false,
        remaining_steps: Vec::new(),
        left_locally_only: false,
        lost_contact: true,
    };
    FinishedPayload {
        job,
        ok: false,
        result_json: serde_json::to_string(&result).unwrap_or_default(),
    }
}

/// What `forward` hands to the window.
#[derive(Debug)]
pub enum Outgoing {
    Progress(ProgressPayload),
    Log(LogPayload),
    Finished(FinishedPayload),
}

/// Sends to the window's `rabita:*` events; false once the window is gone.
fn to_window(app: &AppHandle, out: Outgoing) -> bool {
    match out {
        Outgoing::Progress(p) => app.emit("rabita:progress", p),
        Outgoing::Log(l) => app.emit("rabita:log", l),
        Outgoing::Finished(f) => app.emit("rabita:finished", f),
    }
    .is_ok()
}

/// Forwards one job's signals until it finishes, checking on the helper
/// every `every` so the window never waits forever: if the helper goes
/// away, sits idle without a result, or stops answering, the window gets a
/// "lost contact" result instead.
async fn forward(
    deliver: impl Fn(Outgoing) -> bool,
    mut events: EventStream,
    job: u32,
    proxy: RabitaProxy<'static>,
    owner: Option<OwnedUniqueName>,
    every: Duration,
) {
    let mut watchdog = Watchdog::default();
    let mut ticks = tokio::time::interval(every);
    ticks.tick().await; // the first tick is immediate
    loop {
        tokio::select! {
            event = events.next() => {
                let sent = match event {
                    Some(Event::Progress(p)) if p.job == job => { watchdog.heard(); deliver(Outgoing::Progress(p)) }
                    Some(Event::Log(l)) if l.job == job => { watchdog.heard(); deliver(Outgoing::Log(l)) }
                    Some(Event::Finished(f)) if f.job == job => {
                        deliver(Outgoing::Finished(f));
                        break;
                    }
                    Some(_) => true,
                    None => {
                        deliver(Outgoing::Finished(lost_payload(job, Lost::Gone)));
                        break;
                    }
                };
                if !sent {
                    break;
                }
            }
            _ = ticks.tick() => {
                if let Some(why) = watchdog.saw(check_health(&proxy, owner.as_ref()).await) {
                    deliver(Outgoing::Finished(lost_payload(job, why)));
                    break;
                }
            }
        }
    }
}

/// Subscribes, starts a job with `start`, and forwards its signals.
async fn run_job<F, Fut>(app: AppHandle, bridge: &Bridge, start: F) -> Result<u32, String>
where
    F: FnOnce(RabitaProxy<'static>) -> Fut,
    Fut: Future<Output = zbus::Result<u32>>,
{
    let proxy = bridge.proxy().await?;
    let conn = proxy.inner().connection().clone();
    let owner = current_owner(&conn).await.ok().flatten();
    let events = subscribe(&proxy).await?;
    // The desktop's password window may be open during this call.
    let job = call(&conn, limit(PASSWORD_LIMIT), start(proxy.clone())).await?;
    let every = limit(WATCH_EVERY).min(WATCH_EVERY);
    tauri::async_runtime::spawn(forward(
        move |out| to_window(&app, out),
        events,
        job,
        proxy,
        owner,
        every,
    ));
    Ok(job)
}

/// What kind of computer this is (`SystemInfo` JSON).
#[tauri::command]
pub async fn detect_system(bridge: State<'_, Bridge>) -> Result<String, String> {
    let p = bridge.proxy().await?;
    call(
        p.inner().connection(),
        limit(QUICK_LIMIT),
        p.detect_system(),
    )
    .await
}

/// Finds the domain controllers for a domain (`DiscoveryResult` JSON).
#[tauri::command]
pub async fn discover_domain(bridge: State<'_, Bridge>, domain: String) -> Result<String, String> {
    let p = bridge.proxy().await?;
    call(
        p.inner().connection(),
        limit(SLOW_LIMIT),
        p.discover_domain(&domain),
    )
    .await
}

/// Connection and health report (`StatusReport` JSON).
#[tauri::command]
pub async fn status(bridge: State<'_, Bridge>) -> Result<String, String> {
    let p = bridge.proxy().await?;
    call(p.inner().connection(), limit(SLOW_LIMIT), p.status()).await
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
    run_job(app, &bridge, |p| async move {
        p.join(&request, &password).await
    })
    .await
}

/// Re-applies options, or finishes a setup that stopped after joining.
#[tauri::command]
pub async fn apply_options(
    app: AppHandle,
    bridge: State<'_, Bridge>,
    options: String,
) -> Result<u32, String> {
    run_job(
        app,
        &bridge,
        |p| async move { p.apply_options(&options).await },
    )
    .await
}

/// Leaves the domain (or undoes an unfinished join) and returns the job
/// id. The admin password, if any, is wiped straight after it is sent.
#[tauri::command]
pub async fn leave(
    app: AppHandle,
    bridge: State<'_, Bridge>,
    request: String,
    password: String,
) -> Result<u32, String> {
    let password = Zeroizing::new(password);
    run_job(app, &bridge, |p| async move {
        p.leave(&request, &password).await
    })
    .await
}

/// Looks on this network for DNS servers that know `domain` (`DnsSearch`).
#[tauri::command]
pub async fn find_dns_servers(bridge: State<'_, Bridge>, domain: String) -> Result<String, String> {
    let p = bridge.proxy().await?;
    call(
        p.inner().connection(),
        limit(SLOW_LIMIT),
        p.find_dns_servers(&domain),
    )
    .await
}

/// Makes `server` this computer's DNS server (`DnsResult`); polkit asks for
/// an administrator's password.
#[tauri::command]
pub async fn set_dns_server(
    bridge: State<'_, Bridge>,
    domain: String,
    server: String,
) -> Result<String, String> {
    let p = bridge.proxy().await?;
    call(
        p.inner().connection(),
        limit(PASSWORD_LIMIT),
        p.set_dns_server(&domain, &server),
    )
    .await
}

/// Puts back the DNS setting Rabita changed (`DnsResult`).
#[tauri::command]
pub async fn restore_dns(bridge: State<'_, Bridge>) -> Result<String, String> {
    let p = bridge.proxy().await?;
    call(
        p.inner().connection(),
        limit(PASSWORD_LIMIT),
        p.restore_dns(),
    )
    .await
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
    fn an_older_or_silent_helper_is_explained() {
        assert_eq!(
            friendly_method_error("org.freedesktop.DBus.Error.UnknownMethod", Some("raw")),
            HELPER_WRONG
        );
        assert_eq!(
            friendly_method_error("org.freedesktop.DBus.Error.NoReply", None),
            HELPER_SILENT
        );
        assert_eq!(
            friendly_method_error("org.freedesktop.systemd1.UnitMasked", Some("raw")),
            HELPER_MISSING
        );
        assert!(version_mismatch("0.0.9").contains("version 0.0.9"));
    }

    #[test]
    fn watchdog_waits_while_busy_and_gives_up_when_the_helper_is_gone() {
        let mut w = Watchdog::default();
        for _ in 0..100 {
            assert_eq!(w.saw(Health::Busy), None, "a long apt-get is fine");
        }
        assert_eq!(w.saw(Health::Gone), Some(Lost::Gone));
    }

    #[test]
    fn watchdog_allows_one_idle_check_for_a_result_in_flight() {
        let mut w = Watchdog::default();
        assert_eq!(w.saw(Health::Idle), None);
        w.heard();
        assert_eq!(w.saw(Health::Idle), None, "an event resets the count");
        assert_eq!(w.saw(Health::Idle), Some(Lost::NoResult));
    }

    #[test]
    fn watchdog_gives_up_after_about_a_minute_of_silence() {
        let mut w = Watchdog::default();
        for _ in 1..Watchdog::SILENT_CHECKS {
            assert_eq!(w.saw(Health::NoAnswer), None);
        }
        assert_eq!(w.saw(Health::NoAnswer), Some(Lost::Stuck));
        let total = WATCH_EVERY * Watchdog::SILENT_CHECKS;
        assert!(total >= Duration::from_secs(50) && total <= Duration::from_secs(90));
    }

    #[test]
    fn a_lost_job_still_gives_the_window_a_result() {
        let p = lost_payload(7, Lost::Stuck);
        assert_eq!(p.job, 7);
        assert!(!p.ok);
        let v: serde_json::Value = serde_json::from_str(&p.result_json).unwrap();
        assert_eq!(v["lost_contact"], true);
        assert_eq!(v["ok"], false);
        assert_eq!(v["steps"], serde_json::json!([]));
        assert!(v["summary"].as_str().unwrap().contains("open it again"));
    }

    #[test]
    fn limits_leave_room_for_the_password_window() {
        assert!(
            PASSWORD_LIMIT > Duration::from_secs(5 * 60),
            "the helper closes it at 5 minutes"
        );
        assert!(START_LIMIT < QUICK_LIMIT && QUICK_LIMIT < SLOW_LIMIT);
    }

    /// The job watchdog against a stand-in helper on a private bus.
    mod watchdog_on_a_bus {
        use super::super::*;
        use std::process::{Child, Command, Stdio};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};
        use zbus::object_server::SignalEmitter;

        struct Bus {
            daemon: Child,
            address: String,
            _dir: tempfile::TempDir,
        }

        impl Drop for Bus {
            fn drop(&mut self) {
                let _ = self.daemon.kill();
                let _ = self.daemon.wait();
            }
        }

        /// A private message bus, or None where dbus-daemon is missing.
        fn private_bus() -> Option<Bus> {
            use std::io::BufRead;
            let dir = tempfile::tempdir().ok()?;
            let address = format!("unix:path={}", dir.path().join("bus").display());
            let mut daemon = Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .arg(format!("--address={address}"))
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .ok()?;
            let mut line = String::new();
            std::io::BufReader::new(daemon.stdout.take()?)
                .read_line(&mut line)
                .ok()?;
            Some(Bus {
                daemon,
                address,
                _dir: dir,
            })
        }

        struct FakeHelper {
            busy: Arc<AtomicBool>,
        }

        #[zbus::interface(name = "org.quietbyte.Rabita1")]
        impl FakeHelper {
            #[zbus(property)]
            fn busy(&self) -> bool {
                self.busy.load(Ordering::SeqCst)
            }
            #[zbus(property)]
            #[allow(clippy::unused_self)]
            fn version(&self) -> String {
                APP_VERSION.to_owned()
            }
            #[zbus(signal)]
            async fn progress(
                e: &SignalEmitter<'_>,
                job: u32,
                step: &str,
                state: &str,
                message: &str,
            ) -> zbus::Result<()>;
            #[zbus(signal)]
            async fn log_line(e: &SignalEmitter<'_>, job: u32, line: &str) -> zbus::Result<()>;
            #[zbus(signal)]
            async fn job_finished(
                e: &SignalEmitter<'_>,
                job: u32,
                ok: bool,
                result_json: &str,
            ) -> zbus::Result<()>;
        }

        async fn connect(address: &str) -> Connection {
            zbus::connection::Builder::address(address)
                .unwrap()
                .build()
                .await
                .unwrap()
        }

        async fn helper(address: &str, busy: bool) -> Connection {
            zbus::connection::Builder::address(address)
                .unwrap()
                .name(BUS_NAME)
                .unwrap()
                .serve_at(
                    "/org/quietbyte/Rabita1",
                    FakeHelper {
                        busy: Arc::new(AtomicBool::new(busy)),
                    },
                )
                .unwrap()
                .build()
                .await
                .unwrap()
        }

        type Seen = Arc<Mutex<Vec<Outgoing>>>;

        /// Starts watching job 1 the way `run_job` does; returns what the
        /// window would receive.
        async fn watch(address: &str) -> (Seen, tokio::task::JoinHandle<()>) {
            let conn = connect(address).await;
            let proxy = RabitaProxy::builder(&conn)
                .cache_properties(CacheProperties::No)
                .build()
                .await
                .unwrap();
            let owner = current_owner(&conn).await.unwrap();
            assert!(owner.is_some());
            let events = subscribe(&proxy).await.unwrap();
            let seen: Seen = Arc::default();
            let sink = Arc::clone(&seen);
            let task = tokio::spawn(forward(
                move |o| {
                    sink.lock().unwrap().push(o);
                    true
                },
                events,
                1,
                proxy,
                owner,
                Duration::from_millis(200),
            ));
            (seen, task)
        }

        fn finished(seen: &Seen) -> Option<FinishedPayload> {
            seen.lock().unwrap().iter().find_map(|o| match o {
                Outgoing::Finished(f) => Some(f.clone()),
                _ => None,
            })
        }

        #[tokio::test]
        async fn a_helper_that_goes_away_ends_the_job_with_lost_contact() {
            let Some(bus) = private_bus() else {
                eprintln!("dbus-daemon not installed; skipped");
                return;
            };
            let h = helper(&bus.address, true).await;
            let (seen, task) = watch(&bus.address).await;
            let emitter = SignalEmitter::new(&h, "/org/quietbyte/Rabita1").unwrap();
            FakeHelper::progress(&emitter, 1, "packages", "running", "")
                .await
                .unwrap();
            // Busy for a while: nothing happens.
            tokio::time::sleep(Duration::from_millis(900)).await;
            assert_eq!(finished(&seen), None);
            // The helper crashes.
            drop(emitter);
            h.close().await.unwrap();
            tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .expect("the watchdog gives up within seconds")
                .unwrap();
            let f = finished(&seen).unwrap();
            assert!(f.result_json.contains("\"lost_contact\":true"));
            assert!(f.result_json.contains("stopped or was restarted"));
            assert!(matches!(seen.lock().unwrap()[0], Outgoing::Progress(_)));
        }

        #[tokio::test]
        async fn an_idle_helper_without_a_result_ends_the_job() {
            let Some(bus) = private_bus() else {
                eprintln!("dbus-daemon not installed; skipped");
                return;
            };
            let _h = helper(&bus.address, false).await;
            let (seen, task) = watch(&bus.address).await;
            tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .expect("the watchdog gives up within seconds")
                .unwrap();
            assert!(finished(&seen)
                .unwrap()
                .result_json
                .contains("did not receive the result"));
        }

        #[tokio::test]
        async fn a_normal_result_passes_through_untouched() {
            let Some(bus) = private_bus() else {
                eprintln!("dbus-daemon not installed; skipped");
                return;
            };
            let h = helper(&bus.address, true).await;
            let (seen, task) = watch(&bus.address).await;
            let emitter = SignalEmitter::new(&h, "/org/quietbyte/Rabita1").unwrap();
            // Another job's result is not ours.
            FakeHelper::job_finished(&emitter, 2, true, "{}")
                .await
                .unwrap();
            FakeHelper::job_finished(&emitter, 1, true, "{\"ok\":true}")
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .unwrap()
                .unwrap();
            let f = finished(&seen).unwrap();
            assert_eq!(
                (f.job, f.ok, f.result_json.as_str()),
                (1, true, "{\"ok\":true}")
            );
            assert_eq!(seen.lock().unwrap().len(), 1);
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
