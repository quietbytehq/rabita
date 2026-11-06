// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! `rabita-helper` library: everything the root daemon can do, in testable
//! pieces.
//!
//! The D-Bus service ([`service`]) is a thin shell around [`system`],
//! [`discover`], [`join`], [`leave`] and [`status`]. Those modules never call
//! processes directly; they go through a [`cmd::Runner`] so unit tests can
//! substitute a fake and the real system is never touched from `cargo test`.
//!
//! Rules this crate keeps (see `CLAUDE.md`):
//! - No Samba or SSSD libraries are linked. System tools run as processes.
//! - The admin password is used once, on stdin, and then wiped.
//! - Every file edit is backed up first, and `leave` restores the backups.

pub mod backup;
pub mod client;
pub mod cmd;
pub mod discover;
pub mod dns;
pub mod ini;
pub mod join;
pub mod leave;
pub mod paths;
pub mod polkit;
pub mod progress;
pub mod service;
pub mod sntp;
pub mod status;
pub mod system;
pub mod types;

/// Package version, injected by Cargo at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Well-known D-Bus name of the helper.
pub const BUS_NAME: &str = "org.quietbyte.Rabita1";
/// Object path the helper exports.
pub const OBJECT_PATH: &str = "/org/quietbyte/Rabita1";
/// Interface name.
pub const INTERFACE: &str = "org.quietbyte.Rabita1";
/// polkit action that gates every privileged method.
pub const POLKIT_ACTION: &str = "org.quietbyte.rabita.manage";
