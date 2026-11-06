// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! polkit authorisation for privileged methods.
//!
//! Every call to `Join`, `Leave` or `ApplyOptions` must come from a caller
//! that polkit authorises for `org.quietbyte.rabita.manage`. On a desktop
//! this shows the usual "Authentication required" dialog; root is always
//! authorised.

use std::collections::HashMap;

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
    let result = authority
        .check_authorization(
            &subject,
            POLKIT_ACTION,
            &HashMap::new(),
            CheckAuthorizationFlags::AllowUserInteraction.into(),
            "",
        )
        .await
        .map_err(|e| fdo::Error::AuthFailed(format!("polkit check failed: {e}")))?;
    if result.is_authorized {
        Ok(())
    } else if result.details.contains_key("polkit.dismissed") {
        Err(fdo::Error::AccessDenied(
            "Authentication was cancelled.".into(),
        ))
    } else {
        Err(fdo::Error::AccessDenied(
            "You are not allowed to change this computer's organization connection. An administrator must authorize it.".into(),
        ))
    }
}
