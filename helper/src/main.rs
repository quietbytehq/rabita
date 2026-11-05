// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! `rabita-helper` — the only part of Rabita that runs as root.
//!
//! It is intentionally small and auditable. It exposes
//! `org.quietbyte.Rabita1` on the system D-Bus with the methods
//! `DetectSystem`, `DiscoverDomain`, `Join`, `Leave`, `Status` and
//! `ApplyOptions`. Every privileged method requires polkit authorisation
//! for `org.quietbyte.rabita.manage`.
//!
//! Milestone 1 ships a stub that only reports its version. The D-Bus
//! service and the real methods arrive in Milestone 2.

use rabita_backends::DistroFamily;

/// Package version, injected by Cargo at build time.
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let family =
        detect_family_from_file("/etc/os-release").map_or("unknown", DistroFamily::display_name);
    println!("rabita-helper {VERSION} (stub) — detected family: {family}");
}

/// Reads `/etc/os-release` and maps it to a [`DistroFamily`].
fn detect_family_from_file(path: &str) -> Option<DistroFamily> {
    let contents = std::fs::read_to_string(path).ok()?;
    detect_family(&contents)
}

/// Parses `os-release` text and maps it to a [`DistroFamily`].
fn detect_family(os_release: &str) -> Option<DistroFamily> {
    let mut id = "";
    let mut id_like = "";
    for line in os_release.lines() {
        if let Some(v) = line.strip_prefix("ID=") {
            id = v;
        } else if let Some(v) = line.strip_prefix("ID_LIKE=") {
            id_like = v;
        }
    }
    DistroFamily::from_os_release(id, id_like)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ubuntu_os_release() {
        let text = "NAME=\"Ubuntu\"\nID=ubuntu\nID_LIKE=debian\nVERSION_ID=\"24.04\"\n";
        assert_eq!(detect_family(text), Some(DistroFamily::Debian));
    }

    #[test]
    fn unknown_distro_is_none() {
        assert_eq!(detect_family("ID=nixos\n"), None);
    }
}
