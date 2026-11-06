// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! Per-distribution backends for `rabita-helper`.
//!
//! A backend never touches the system itself. It turns a request ("install
//! the join packages", "enable home-directory creation", "show a manual
//! username box on the login screen") into a list of [`Action`]s. The helper
//! executes those actions, backing up every file first so that "Leave domain"
//! can put everything back.
//!
//! Keeping backends pure makes them easy to unit-test without root and easy
//! to audit: the full list of system changes for a distribution is the output
//! of a handful of functions.

pub mod arch;
pub mod debian;
pub mod fedora;
pub mod suse;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The Linux families Rabita knows how to configure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
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

    /// Returns the backend for this family.
    #[must_use]
    pub fn backend(self) -> Box<dyn Backend> {
        match self {
            Self::Debian => Box::new(debian::Debian),
            Self::Fedora => Box::new(fedora::Fedora),
            Self::Suse => Box::new(suse::Suse),
            Self::Arch => Box::new(arch::Arch),
        }
    }
}

/// The display manager (login screen) in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DisplayManager {
    /// GNOME Display Manager (`gdm` / `gdm3`).
    Gdm,
    /// Simple Desktop Display Manager (KDE Plasma).
    Sddm,
    /// `LightDM` (Linux Mint, Xubuntu, Lubuntu and others).
    LightDm,
    /// Something else, or none detected.
    Unknown,
}

impl DisplayManager {
    /// Maps a systemd unit name such as `gdm3.service` to a display manager.
    #[must_use]
    pub fn from_unit_name(unit: &str) -> Self {
        let name = unit.trim().trim_end_matches(".service");
        match name {
            "gdm" | "gdm3" => Self::Gdm,
            "sddm" => Self::Sddm,
            "lightdm" => Self::LightDm,
            _ => Self::Unknown,
        }
    }
}

/// Facts about the machine that a backend needs in order to plan.
///
/// The helper gathers these; backends never read the filesystem themselves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemFacts {
    /// The display manager in use.
    pub display_manager: Option<DisplayManager>,
    /// Whether a `mkhomedir` PAM profile is already shipped by the
    /// distribution (Debian family only).
    pub has_stock_mkhomedir_profile: bool,
    /// Whether home-directory creation is already active in PAM before
    /// Rabita touched anything.
    pub mkhomedir_already_enabled: bool,
    /// Whether a dconf profile for GDM already exists (either in `/etc` or
    /// `/usr/share`).
    pub gdm_dconf_profile_exists: bool,
    /// Path of the distribution's compiled GDM defaults database, if any
    /// (Ubuntu ships `/var/lib/gdm3/greeter-dconf-defaults`).
    pub gdm_defaults_db: Option<PathBuf>,
}

/// One concrete change to the system, planned by a backend and executed by
/// the helper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Run a program. `stdin` is written to the process and never logged.
    Run {
        /// Plain-language description shown in the wizard log.
        description: String,
        /// Program name (looked up on `PATH`).
        program: String,
        /// Arguments. Secrets must never appear here.
        args: Vec<String>,
        /// Extra environment variables.
        env: Vec<(String, String)>,
        /// Data to feed on standard input.
        stdin: Option<String>,
        /// Whether a non-zero exit is fatal for the step.
        required: bool,
    },
    /// Create or replace a file. The helper backs up the previous version.
    WriteFile {
        /// Plain-language description shown in the wizard log.
        description: String,
        /// Absolute path.
        path: PathBuf,
        /// Full file contents.
        content: String,
        /// Unix permission bits.
        mode: u32,
    },
    /// Delete a file that Rabita created earlier (used when leaving).
    RemoveFile {
        /// Plain-language description shown in the wizard log.
        description: String,
        /// Absolute path.
        path: PathBuf,
    },
}

impl Action {
    /// Convenience constructor for a required command.
    #[must_use]
    pub fn run(description: &str, program: &str, args: &[&str]) -> Self {
        Self::Run {
            description: description.to_owned(),
            program: program.to_owned(),
            args: args.iter().map(|s| (*s).to_owned()).collect(),
            env: Vec::new(),
            stdin: None,
            required: true,
        }
    }

    /// Convenience constructor for a command whose failure is not fatal.
    #[must_use]
    pub fn run_optional(description: &str, program: &str, args: &[&str]) -> Self {
        match Self::run(description, program, args) {
            Self::Run {
                description,
                program,
                args,
                env,
                stdin,
                ..
            } => Self::Run {
                description,
                program,
                args,
                env,
                stdin,
                required: false,
            },
            other => other,
        }
    }

    /// Convenience constructor for a configuration file (mode 0644).
    #[must_use]
    pub fn write_file(description: &str, path: &str, content: &str) -> Self {
        Self::WriteFile {
            description: description.to_owned(),
            path: PathBuf::from(path),
            content: content.to_owned(),
            mode: 0o644,
        }
    }

    /// Convenience constructor for removing a file Rabita created.
    #[must_use]
    pub fn remove_file(description: &str, path: &str) -> Self {
        Self::RemoveFile {
            description: description.to_owned(),
            path: PathBuf::from(path),
        }
    }

    /// The plain-language description of this action.
    #[must_use]
    pub fn description(&self) -> &str {
        match self {
            Self::Run { description, .. }
            | Self::WriteFile { description, .. }
            | Self::RemoveFile { description, .. } => description,
        }
    }
}

/// What a backend can plan for its distribution family.
pub trait Backend: Send + Sync {
    /// The family this backend serves.
    fn family(&self) -> DistroFamily;

    /// Whether this backend is implemented yet. Unimplemented backends make
    /// the wizard show "Not supported yet" and change nothing.
    fn is_implemented(&self) -> bool;

    /// Packages needed for domain login, drives and printers.
    fn packages(&self) -> Vec<&'static str>;

    /// Actions that install `packages` without prompting. `realm` is the
    /// Kerberos realm (upper-case domain) for pre-seeding configuration.
    fn install_packages(&self, packages: &[&str], realm: &str) -> Vec<Action>;

    /// Actions that make the login stack use SSSD and create home
    /// directories on first login.
    fn enable_login_integration(&self, facts: &SystemFacts) -> Vec<Action>;

    /// Actions that undo [`Backend::enable_login_integration`].
    fn disable_login_integration(&self, facts: &SystemFacts) -> Vec<Action>;

    /// Actions that let people type a username at the login screen
    /// ("Not listed?" / "Other user").
    fn enable_manual_login(&self, facts: &SystemFacts) -> Vec<Action>;

    /// Actions that undo [`Backend::enable_manual_login`].
    fn disable_manual_login(&self, facts: &SystemFacts) -> Vec<Action>;
}

/// Shared helper for backends that are not implemented yet.
pub(crate) fn not_implemented() -> Vec<Action> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::{DisplayManager, DistroFamily};

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

    #[test]
    fn display_manager_from_unit() {
        assert_eq!(
            DisplayManager::from_unit_name("gdm3.service"),
            DisplayManager::Gdm
        );
        assert_eq!(
            DisplayManager::from_unit_name("gdm.service"),
            DisplayManager::Gdm
        );
        assert_eq!(
            DisplayManager::from_unit_name("lightdm.service"),
            DisplayManager::LightDm
        );
        assert_eq!(
            DisplayManager::from_unit_name("sddm.service"),
            DisplayManager::Sddm
        );
        assert_eq!(
            DisplayManager::from_unit_name("ly.service"),
            DisplayManager::Unknown
        );
    }

    #[test]
    fn only_debian_is_implemented_yet() {
        assert!(DistroFamily::Debian.backend().is_implemented());
        assert!(!DistroFamily::Fedora.backend().is_implemented());
        assert!(!DistroFamily::Suse.backend().is_implemented());
        assert!(!DistroFamily::Arch.backend().is_implemented());
    }
}
