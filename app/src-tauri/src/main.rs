// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! `rabita-app` — the "Rabita Setup" wizard window.
//!
//! This is the Rust side of the Tauri 2 application. It never runs as root;
//! all privileged work is requested from `rabita-helper` over D-Bus.
//!
//! Milestone 1 ships a stub. The six wizard screens are wired up in
//! Milestone 3.

/// Package version, injected by Cargo at build time.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The wizard steps, in order, as shown in the left banner.
const STEPS: [&str; 6] = [
    "Welcome",
    "Your organization",
    "IT admin sign-in",
    "Choose options",
    "Setting up",
    "Finish",
];

fn main() {
    println!("rabita-app {VERSION} (stub)");
    for (i, step) in STEPS.iter().enumerate() {
        println!("  {}. {step}", i + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::STEPS;

    #[test]
    fn wizard_has_six_steps_in_order() {
        assert_eq!(STEPS.first(), Some(&"Welcome"));
        assert_eq!(STEPS.last(), Some(&"Finish"));
        assert_eq!(STEPS.len(), 6);
    }
}
