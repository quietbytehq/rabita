// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! Per-distribution backends for `rabita-helper`.
//!
//! Each backend knows how a particular Linux family installs packages, hooks
//! SSSD into PAM/NSS, and configures the display manager for manual username
//! entry. The helper picks one backend at runtime from [`DistroFamily`].
//!
//! Milestone 1 ships only the family detection skeleton. The real steps land
//! with Milestone 2 (Debian/Ubuntu) and Milestone 7 (the other families).

pub mod arch;
pub mod debian;
pub mod fedora;
pub mod suse;

/// The Linux families Rabita knows how to configure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistroFamily {
    /// Debian, Ubuntu, Linux Mint and derivatives (`apt`, `pam-auth-update`).
    Debian,
    /// Fedora, RHEL, Rocky, Alma Linux (`dnf`, `authselect`).
    Fedora,
    /// openSUSE Leap and Tumbleweed (`zypper`, `pam-config`).
    Suse,
    /// Arch Linux and Manjaro (`pacman`, manual PAM edits).
    Arch,
}

impl DistroFamily {
    /// Maps the `ID` / `ID_LIKE` fields of `/etc/os-release` to a family.
    ///
    /// Returns `None` for distributions Rabita does not support, so the
    /// wizard can show "Not supported yet" and change nothing.
    #[must_use]
    pub fn from_os_release(id: &str, id_like: &str) -> Option<Self> {
        let candidates = std::iter::once(id).chain(id_like.split_whitespace());
        for name in candidates {
            match name.trim().trim_matches('"').to_ascii_lowercase().as_str() {
                "debian" | "ubuntu" | "linuxmint" | "pop" => return Some(Self::Debian),
                "fedora" | "rhel" | "centos" | "rocky" | "almalinux" => return Some(Self::Fedora),
                "opensuse" | "opensuse-leap" | "opensuse-tumbleweed" | "suse" | "sles" => {
                    return Some(Self::Suse);
                }
                "arch" | "archlinux" | "manjaro" | "endeavouros" => return Some(Self::Arch),
                _ => {}
            }
        }
        None
    }

    /// Human-readable name used in the wizard and logs.
    #[must_use]
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Debian => "Debian / Ubuntu",
            Self::Fedora => "Fedora / RHEL",
            Self::Suse => "openSUSE",
            Self::Arch => "Arch Linux",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DistroFamily;

    #[test]
    fn detects_ubuntu_via_id_like() {
        assert_eq!(
            DistroFamily::from_os_release("ubuntu", "debian"),
            Some(DistroFamily::Debian)
        );
    }

    #[test]
    fn detects_mint_via_id_like_chain() {
        assert_eq!(
            DistroFamily::from_os_release("linuxmint", "ubuntu debian"),
            Some(DistroFamily::Debian)
        );
    }

    #[test]
    fn detects_rocky_via_id_like() {
        assert_eq!(
            DistroFamily::from_os_release("rocky", "\"rhel centos fedora\""),
            Some(DistroFamily::Fedora)
        );
    }

    #[test]
    fn detects_tumbleweed() {
        assert_eq!(
            DistroFamily::from_os_release("opensuse-tumbleweed", "opensuse suse"),
            Some(DistroFamily::Suse)
        );
    }

    #[test]
    fn detects_manjaro() {
        assert_eq!(
            DistroFamily::from_os_release("manjaro", "arch"),
            Some(DistroFamily::Arch)
        );
    }

    #[test]
    fn rejects_unsupported() {
        assert_eq!(DistroFamily::from_os_release("nixos", ""), None);
        assert_eq!(DistroFamily::from_os_release("alpine", ""), None);
    }
}
