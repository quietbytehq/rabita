// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! `DetectSystem`: what kind of computer is this?

use std::collections::HashMap;
use std::fs;

use rabita_backends::{DisplayManager, DistroFamily, SystemFacts};

use crate::backup::Store;
use crate::cmd::{CommandSpec, Runner};
use crate::paths::Paths;
use crate::types::SystemInfo;

/// Parses `/etc/os-release` text into key/value pairs (quotes removed).
#[must_use]
pub fn parse_os_release(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.starts_with('#') {
                return None;
            }
            let (k, v) = l.split_once('=')?;
            Some((k.trim().to_owned(), v.trim().trim_matches('"').to_owned()))
        })
        .collect()
}

/// One entry from `realm list`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RealmEntry {
    /// Realm name (first line of the block).
    pub name: String,
    /// `domain-name:` value.
    pub domain_name: Option<String>,
    /// `configured:` value (e.g. `kerberos-member`, `no`).
    pub configured: Option<String>,
    /// `client-software:` value (e.g. `sssd`).
    pub client_software: Option<String>,
}

impl RealmEntry {
    /// Whether this computer is a member of the realm.
    #[must_use]
    pub fn is_member(&self) -> bool {
        self.configured
            .as_deref()
            .is_some_and(|c| c != "no" && !c.is_empty())
    }
}

/// Parses the output of `realm list`.
#[must_use]
pub fn parse_realm_list(text: &str) -> Vec<RealmEntry> {
    let mut out: Vec<RealmEntry> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('\t') {
            out.push(RealmEntry {
                name: line.trim().to_owned(),
                ..RealmEntry::default()
            });
            continue;
        }
        let Some(current) = out.last_mut() else {
            continue;
        };
        if let Some((k, v)) = line.trim().split_once(':') {
            let v = v.trim().to_owned();
            match k.trim() {
                "domain-name" => current.domain_name = Some(v),
                "configured" => current.configured = Some(v),
                "client-software" => current.client_software = Some(v),
                _ => {}
            }
        }
    }
    out
}

/// The domain this computer is joined to, according to `realm list`.
#[must_use]
pub fn joined_realm(runner: &dyn Runner) -> Option<RealmEntry> {
    if !runner.which("realm") {
        return None;
    }
    let out = runner
        .run(&CommandSpec::new("realm", &["list"]))
        .ok()
        .filter(crate::cmd::CommandOutput::success)?;
    parse_realm_list(&out.stdout)
        .into_iter()
        .find(RealmEntry::is_member)
}

/// Detects the display manager from the systemd `display-manager.service`
/// link.
#[must_use]
pub fn detect_display_manager(paths: &Paths) -> DisplayManager {
    match fs::read_link(paths.display_manager_link()) {
        Ok(target) => target.file_name().map_or(DisplayManager::Unknown, |n| {
            DisplayManager::from_unit_name(&n.to_string_lossy())
        }),
        Err(_) => DisplayManager::Unknown,
    }
}

/// Gathers the facts backends need to plan.
#[must_use]
pub fn gather_facts(paths: &Paths) -> SystemFacts {
    let pam_session = fs::read_to_string(paths.pam_common_session()).unwrap_or_default();
    SystemFacts {
        display_manager: Some(detect_display_manager(paths)),
        has_stock_mkhomedir_profile: paths.stock_mkhomedir_profile().is_file(),
        mkhomedir_already_enabled: pam_session.contains("pam_mkhomedir.so"),
        gdm_dconf_profile_exists: paths.gdm_dconf_profiles().iter().any(|p| p.is_file()),
        gdm_defaults_db: Paths::gdm_defaults_db_candidates()
            .into_iter()
            .find(|p| paths.resolve(p).is_file())
            .map(std::path::Path::to_path_buf),
    }
}

/// Runs `DetectSystem`.
#[must_use]
pub fn detect(runner: &dyn Runner, paths: &Paths) -> SystemInfo {
    let os = parse_os_release(&fs::read_to_string(paths.os_release()).unwrap_or_default());
    let id = os.get("ID").cloned().unwrap_or_default();
    let id_like = os.get("ID_LIKE").cloned().unwrap_or_default();
    let family = DistroFamily::from_os_release(&id, &id_like);

    let mut unsupported_reason = None;
    if paths.ostree_marker().exists() {
        unsupported_reason = Some(
            "This is an immutable system (such as Silverblue or Kinoite). Rabita cannot change its login setup yet.".to_owned(),
        );
    } else if family.is_none() {
        unsupported_reason = Some(format!(
            "Rabita does not support {} yet.",
            os.get("PRETTY_NAME")
                .cloned()
                .unwrap_or_else(|| "this Linux distribution".to_owned())
        ));
    } else if let Some(f) = family {
        if !f.backend().is_implemented() {
            unsupported_reason = Some(format!(
                "Support for {} is coming in a later release.",
                f.display_name()
            ));
        }
    }

    let hostname = fs::read_to_string(paths.hostname_file())
        .map(|s| s.trim().to_owned())
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            runner
                .run(&CommandSpec::new("hostname", &[]))
                .ok()
                .map(|o| o.stdout.trim().to_owned())
        })
        .unwrap_or_else(|| "localhost".to_owned());

    let store = Store::new(paths.clone());
    let joined_domain = store
        .load()
        .ok()
        .filter(|s| s.joined)
        .and_then(|s| s.domain)
        .or_else(|| joined_realm(runner).and_then(|r| r.domain_name.or(Some(r.name))));

    SystemInfo {
        distro_id: id,
        distro_name: os.get("PRETTY_NAME").cloned().unwrap_or_default(),
        version_id: os.get("VERSION_ID").cloned().unwrap_or_default(),
        family,
        supported: unsupported_reason.is_none(),
        unsupported_reason,
        display_manager: detect_display_manager(paths),
        hostname,
        joined_domain,
        realmd_installed: runner.which("realm"),
        sssd_installed: runner.which("sssd"),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;
    use crate::cmd::FakeRunner;

    const REALM_LIST: &str = "\
lab.quietbyte.test
  type: kerberos
  realm-name: LAB.QUIETBYTE.TEST
  domain-name: lab.quietbyte.test
  configured: kerberos-member
  server-software: active-directory
  client-software: sssd
  required-package: sssd-tools
  login-formats: %U
  login-policy: allow-realm-logins
";

    #[test]
    fn parses_realm_list() {
        let entries = parse_realm_list(REALM_LIST);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "lab.quietbyte.test");
        assert_eq!(
            entries[0].domain_name.as_deref(),
            Some("lab.quietbyte.test")
        );
        assert!(entries[0].is_member());
        assert_eq!(entries[0].client_software.as_deref(), Some("sssd"));
        let not = parse_realm_list("other.test\n  configured: no\n");
        assert!(!not[0].is_member());
        assert_eq!(parse_realm_list(""), Vec::<RealmEntry>::new());
    }

    fn write(paths: &Paths, abs: &str, content: &str) {
        let p = paths.resolve(std::path::Path::new(abs));
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    #[test]
    fn detects_ubuntu_with_gdm() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        write(
            &paths,
            "/etc/os-release",
            "PRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\nNAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nID=ubuntu\nID_LIKE=debian\n",
        );
        write(&paths, "/etc/hostname", "lab-pc-01\n");
        let link = paths.display_manager_link();
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink("/usr/lib/systemd/system/gdm3.service", &link).unwrap();

        let fake = FakeRunner::new();
        fake.installed(&["realm"]).ok("realm list", "");
        let info = detect(&fake, &paths);
        assert_eq!(info.distro_id, "ubuntu");
        assert_eq!(info.version_id, "24.04");
        assert_eq!(info.family, Some(DistroFamily::Debian));
        assert!(info.supported, "{:?}", info.unsupported_reason);
        assert_eq!(info.display_manager, DisplayManager::Gdm);
        assert_eq!(info.hostname, "lab-pc-01");
        assert_eq!(info.joined_domain, None);
        assert!(info.realmd_installed);
        assert!(!info.sssd_installed);
    }

    #[test]
    fn reports_joined_domain_from_realm_list() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        write(
            &paths,
            "/etc/os-release",
            "ID=ubuntu\nID_LIKE=debian\nPRETTY_NAME=Ubuntu\n",
        );
        let fake = FakeRunner::new();
        fake.ok("realm list", REALM_LIST);
        let info = detect(&fake, &paths);
        assert_eq!(info.joined_domain.as_deref(), Some("lab.quietbyte.test"));
    }

    #[test]
    fn unsupported_distros_are_reported_in_plain_language() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        write(
            &paths,
            "/etc/os-release",
            "ID=nixos\nPRETTY_NAME=\"NixOS 24.05\"\n",
        );
        let info = detect(&FakeRunner::new(), &paths);
        assert!(!info.supported);
        assert_eq!(
            info.unsupported_reason.as_deref(),
            Some("Rabita does not support NixOS 24.05 yet.")
        );

        write(
            &paths,
            "/etc/os-release",
            "ID=fedora\nPRETTY_NAME=\"Fedora 41\"\n",
        );
        let info = detect(&FakeRunner::new(), &paths);
        assert!(!info.supported);
        assert!(info.unsupported_reason.unwrap().contains("later release"));

        write(&paths, "/run/ostree-booted", "");
        let info = detect(&FakeRunner::new(), &paths);
        assert!(info.unsupported_reason.unwrap().contains("immutable"));
    }

    #[test]
    fn facts_reflect_pam_and_gdm_files() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        write(
            &paths,
            "/etc/pam.d/common-session",
            "session optional pam_mkhomedir.so\n",
        );
        write(&paths, "/usr/share/pam-configs/mkhomedir", "Name: x\n");
        write(&paths, "/var/lib/gdm3/greeter-dconf-defaults", "");
        let facts = gather_facts(&paths);
        assert!(facts.mkhomedir_already_enabled);
        assert!(facts.has_stock_mkhomedir_profile);
        assert!(!facts.gdm_dconf_profile_exists);
        assert_eq!(
            facts.gdm_defaults_db.as_deref(),
            Some(std::path::Path::new("/var/lib/gdm3/greeter-dconf-defaults"))
        );
        assert_eq!(facts.display_manager, Some(DisplayManager::Unknown));
    }
}
