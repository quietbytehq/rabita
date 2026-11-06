// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! `rabita-agent` — runs in the user's session after login.
//!
//! It reads the user's drive and printer assignments (LDAP home drive and
//! GPO Preferences), mounts shares with Kerberos, adds printers to CUPS and
//! creates file-manager bookmarks such as `H: Home`. It never runs as root.
//!
//! Milestone 1 ships a stub that only reports its version. Drive mapping
//! arrives in Milestone 4 and printers in Milestone 5.

/// Package version, injected by Cargo at build time.
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    println!("rabita-agent {VERSION} (stub)");
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/user".to_owned());
    println!("  example mount point: {}", mount_point(&home, 'H', "Home"));
}

/// Builds the mount point for a drive letter, e.g. `H` + `Home` →
/// `<home>/Network/H-Home`.
#[must_use]
fn mount_point(home: &str, letter: char, label: &str) -> String {
    let safe_label: String = label
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    format!(
        "{home}/Network/{}-{safe_label}",
        letter.to_ascii_uppercase()
    )
}

#[cfg(test)]
mod tests {
    use super::mount_point;

    #[test]
    fn builds_mount_point() {
        assert_eq!(
            mount_point("/home/amna", 'h', "Home"),
            "/home/amna/Network/H-Home"
        );
    }

    #[test]
    fn sanitises_label() {
        assert_eq!(
            mount_point("/home/amna", 'S', "Staff Share"),
            "/home/amna/Network/S-Staff-Share"
        );
    }
}
