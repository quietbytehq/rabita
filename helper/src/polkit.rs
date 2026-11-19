// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! polkit authorisation for privileged methods.
//!
//! Every call to `Join`, `Leave` or `ApplyOptions` must come from a caller
//! that polkit authorises for `org.quietbyte.rabita.manage`. On a desktop
//! this shows the usual "Authentication required" dialog; root is always
//! authorised.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use zbus::message::Header;
use zbus::names::BusName;
use zbus::{fdo, Connection};
use zbus_polkit::policykit1::{AuthorityProxy, CheckAuthorizationFlags, Subject};

use crate::POLKIT_ACTION;

/// Where the helper is listening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bus {
    /// The system bus (production).
    System,
    /// The session bus (development only; still checks polkit by process id).
    Session,
}

/// How long the desktop's password window may stay open unanswered. After
/// this the helper closes it and refuses, so a forgotten dialog can't leave
/// the wizard waiting forever (the wizard gives up a little later).
pub const PASSWORD_WINDOW_LIMIT: Duration = Duration::from_secs(5 * 60);

/// Message when nobody answers the password window in time.
pub const PASSWORD_WINDOW_TIMED_OUT: &str =
    "Nobody answered the password window within 5 minutes, so nothing was changed. Try again when you are ready to enter the password.";

static NEXT_CHECK: AtomicU64 = AtomicU64::new(1);

/// Checks that the sender of `header` may manage the domain connection.
///
/// `conn` is the connection the request arrived on; polkit itself is always
/// asked over the system bus.
pub async fn require_manage(conn: &Connection, bus: Bus, header: &Header<'_>) -> fdo::Result<()> {
    let subject = match bus {
        Bus::System => Subject::new_for_message_header(header)
            .map_err(|e| fdo::Error::AuthFailed(format!("cannot identify caller: {e}")))?,
        Bus::Session => {
            let sender = header
                .sender()
                .ok_or_else(|| fdo::Error::AuthFailed("no sender".into()))?;
            let dbus = fdo::DBusProxy::new(conn).await?;
            let pid = dbus
                .get_connection_unix_process_id(BusName::Unique(sender.to_owned()))
                .await?;
            Subject::new_for_owner(pid, None, None)
                .map_err(|e| fdo::Error::AuthFailed(format!("cannot identify caller: {e}")))?
        }
    };
    let system = match bus {
        Bus::System => conn.clone(),
        Bus::Session => Connection::system()
            .await
            .map_err(|e| fdo::Error::AuthFailed(format!("polkit unavailable: {e}")))?,
    };
    let authority = AuthorityProxy::new(&system)
        .await
        .map_err(|e| fdo::Error::AuthFailed(format!("polkit unavailable: {e}")))?;
    let cancel_id = format!(
        "rabita-{}-{}",
        std::process::id(),
        NEXT_CHECK.fetch_add(1, Ordering::Relaxed)
    );
    let details = HashMap::new();
    let check = authority.check_authorization(
        &subject,
        POLKIT_ACTION,
        &details,
        CheckAuthorizationFlags::AllowUserInteraction.into(),
        &cancel_id,
    );
    let Ok(result) = tokio::time::timeout(PASSWORD_WINDOW_LIMIT, check).await else {
        // Close the dialog; polkit then answers the pending check.
        let _ = authority.cancel_check_authorization(&cancel_id).await;
        return Err(fdo::Error::AccessDenied(PASSWORD_WINDOW_TIMED_OUT.into()));
    };
    let result = result.map_err(|e| fdo::Error::AuthFailed(format!("polkit check failed: {e}")))?;
    if result.is_authorized {
        Ok(())
    } else if result.details.contains_key("polkit.dismissed") {
        Err(fdo::Error::AccessDenied(
            "Authentication was cancelled.".into(),
        ))
    } else if result.is_challenge {
        // An administrator could have allowed it, but no password dialog
        // (polkit authentication agent) was there to ask.
        Err(fdo::Error::AccessDenied(
            "No password dialog could be shown, so Rabita could not ask for permission. Open Rabita Setup from the desktop's app menu while signed in to the desktop (not from a remote session), then try again.".into(),
        ))
    } else {
        Err(fdo::Error::AccessDenied(
            "You are not allowed to change this computer's organization connection. An administrator must authorize it.".into(),
        ))
    }
}
