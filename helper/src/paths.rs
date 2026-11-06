// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! Where things live on disk.
//!
//! All paths hang off a root (`/` in production) so unit tests can point the
//! helper at a temporary directory and exercise real file handling.

use std::path::{Path, PathBuf};

/// Filesystem locations used by the helper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    root: PathBuf,
}

impl Paths {
    /// The real system (`/`).
    #[must_use]
    pub fn system() -> Self {
        Self {
            root: PathBuf::from("/"),
        }
    }

    /// Everything relative to `root` (for tests).
    #[must_use]
    pub fn under(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    /// Maps an absolute system path (`/etc/x`) under this root.
    #[must_use]
    pub fn resolve(&self, absolute: &Path) -> PathBuf {
        let rel = absolute.strip_prefix("/").unwrap_or(absolute);
        self.root.join(rel)
    }

    /// `/etc/os-release`.
    #[must_use]
    pub fn os_release(&self) -> PathBuf {
        self.resolve(Path::new("/etc/os-release"))
    }

    /// `/etc/sssd/sssd.conf`.
    #[must_use]
    pub fn sssd_conf(&self) -> PathBuf {
        self.resolve(Path::new("/etc/sssd/sssd.conf"))
    }

    /// `/etc/krb5.keytab`.
    #[must_use]
    pub fn keytab(&self) -> PathBuf {
        self.resolve(Path::new("/etc/krb5.keytab"))
    }

    /// `/etc/krb5.conf`.
    #[must_use]
    pub fn krb5_conf(&self) -> PathBuf {
        self.resolve(Path::new("/etc/krb5.conf"))
    }

    /// `/etc/pam.d/common-session` (Debian family).
    #[must_use]
    pub fn pam_common_session(&self) -> PathBuf {
        self.resolve(Path::new("/etc/pam.d/common-session"))
    }

    /// `/etc/systemd/system/display-manager.service` symlink.
    #[must_use]
    pub fn display_manager_link(&self) -> PathBuf {
        self.resolve(Path::new("/etc/systemd/system/display-manager.service"))
    }

    /// `/var/lib/rabita`.
    #[must_use]
    pub fn state_dir(&self) -> PathBuf {
        self.resolve(Path::new("/var/lib/rabita"))
    }

    /// `/var/lib/rabita/state.json`.
    #[must_use]
    pub fn state_file(&self) -> PathBuf {
        self.state_dir().join("state.json")
    }

    /// `/var/lib/rabita/backups`.
    #[must_use]
    pub fn backup_dir(&self) -> PathBuf {
        self.state_dir().join("backups")
    }

    /// `/etc/systemd/timesyncd.conf.d/90-rabita.conf`.
    #[must_use]
    pub fn timesyncd_dropin(&self) -> PathBuf {
        self.resolve(Path::new("/etc/systemd/timesyncd.conf.d/90-rabita.conf"))
    }

    /// `/etc/chrony/sources.d/rabita.sources`.
    #[must_use]
    pub fn chrony_sources(&self) -> PathBuf {
        self.resolve(Path::new("/etc/chrony/sources.d/rabita.sources"))
    }

    /// `/run/ostree-booted` (present on immutable distributions).
    #[must_use]
    pub fn ostree_marker(&self) -> PathBuf {
        self.resolve(Path::new("/run/ostree-booted"))
    }

    /// `/etc/hostname`.
    #[must_use]
    pub fn hostname_file(&self) -> PathBuf {
        self.resolve(Path::new("/etc/hostname"))
    }

    /// `/etc/resolv.conf`.
    #[must_use]
    pub fn resolv_conf(&self) -> PathBuf {
        self.resolve(Path::new("/etc/resolv.conf"))
    }

    /// `/usr/share/pam-configs/mkhomedir` (stock profile, if the distro ships it).
    #[must_use]
    pub fn stock_mkhomedir_profile(&self) -> PathBuf {
        self.resolve(Path::new("/usr/share/pam-configs/mkhomedir"))
    }

    /// Candidate GDM dconf profiles.
    #[must_use]
    pub fn gdm_dconf_profiles(&self) -> [PathBuf; 2] {
        [
            self.resolve(Path::new("/etc/dconf/profile/gdm")),
            self.resolve(Path::new("/usr/share/dconf/profile/gdm")),
        ]
    }

    /// Candidate compiled GDM defaults databases, as system paths. Use
    /// [`Paths::resolve`] to check for them.
    #[must_use]
    pub fn gdm_defaults_db_candidates() -> [&'static Path; 2] {
        [
            Path::new("/var/lib/gdm3/greeter-dconf-defaults"),
            Path::new("/usr/share/gdm/greeter-dconf-defaults"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_under_root() {
        let p = Paths::under(Path::new("/tmp/x"));
        assert_eq!(p.sssd_conf(), PathBuf::from("/tmp/x/etc/sssd/sssd.conf"));
        assert_eq!(
            Paths::system().sssd_conf(),
            PathBuf::from("/etc/sssd/sssd.conf")
        );
        assert_eq!(p.resolve(Path::new("/a/b")), PathBuf::from("/tmp/x/a/b"));
    }
}
