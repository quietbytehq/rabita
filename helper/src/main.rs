// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! `rabita-helper` — the only part of Rabita that runs as root.
//!
//! It owns `org.quietbyte.Rabita1` on the system D-Bus and does the actual
//! joining, leaving and status checks. Every privileged method is gated by
//! polkit (`org.quietbyte.rabita.manage`).

use clap::Parser;
use rabita_helper::polkit::Bus;
use rabita_helper::service::RabitaService;
use rabita_helper::{BUS_NAME, OBJECT_PATH, VERSION};

#[derive(Parser, Debug)]
#[command(name = "rabita-helper", version = VERSION, about = "Rabita root helper (D-Bus service)")]
struct Args {
    /// Listen on the session bus instead of the system bus (development
    /// only). polkit is still consulted, by process id.
    #[arg(long)]
    session: bool,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let bus = if args.session {
        Bus::Session
    } else {
        Bus::System
    };
    if bus == Bus::System && !nix_is_root() {
        eprintln!(
            "rabita-helper: must run as root on the system bus (or use --session for development)"
        );
        std::process::exit(2);
    }
    let service = RabitaService::new(bus);
    let builder = match bus {
        Bus::System => zbus::connection::Builder::system()?,
        Bus::Session => zbus::connection::Builder::session()?,
    };
    let _conn = builder
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, service)?
        .build()
        .await?;
    eprintln!(
        "rabita-helper {VERSION}: serving {BUS_NAME} on the {} bus",
        if args.session { "session" } else { "system" }
    );

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
    eprintln!("rabita-helper: shutting down");
    Ok(())
}

fn nix_is_root() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(1).map(|u| u == "0"))
        })
        .unwrap_or(false)
}
