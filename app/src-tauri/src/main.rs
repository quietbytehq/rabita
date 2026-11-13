// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! `rabita-app` — the "Rabita Setup" wizard window.
//!
//! This is the Rust side of the Tauri 2 application. It never runs as root:
//! everything privileged is asked of `rabita-helper` over D-Bus, and polkit
//! shows the usual password dialog when the helper needs permission.

mod bridge;

fn main() {
    if bridge::running_as_root() {
        eprintln!(
            "Rabita Setup must not be run as root. Open it as a normal user; it asks for an administrator password when it needs one."
        );
        std::process::exit(1);
    }
    let result = tauri::Builder::default()
        .manage(bridge::Bridge::default())
        .invoke_handler(tauri::generate_handler![
            bridge::detect_system,
            bridge::discover_domain,
            bridge::status,
            bridge::join,
            bridge::apply_options,
            bridge::quit,
        ])
        .run(tauri::generate_context!());
    if let Err(e) = result {
        eprintln!("Rabita Setup could not start: {e}");
        std::process::exit(1);
    }
}
