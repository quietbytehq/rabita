// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! Backend for Debian, Ubuntu and Linux Mint.
//!
//! Tools used: `apt-get`, `debconf-set-selections`, `pam-auth-update`,
//! `dconf` (GDM) and `LightDM` configuration drop-ins. Everything runs
//! non-interactively.

use crate::{not_implemented, Action, Backend, DisplayManager, DistroFamily, SystemFacts};

/// Debian-family backend.
#[derive(Debug, Default, Clone, Copy)]
pub struct Debian;

/// PAM profile Rabita installs when the distribution does not ship one.
pub const RABITA_MKHOMEDIR_PROFILE: &str = "rabita-mkhomedir";
/// Path of that profile.
pub const RABITA_MKHOMEDIR_PATH: &str = "/usr/share/pam-configs/rabita-mkhomedir";
/// dconf profile for the GDM login screen.
pub const GDM_DCONF_PROFILE: &str = "/etc/dconf/profile/gdm";
/// dconf key-file drop-in that hides the user list on GDM.
pub const GDM_DCONF_DROPIN: &str = "/etc/dconf/db/gdm.d/90-rabita-login";
/// `LightDM` drop-in that shows the manual login box.
pub const LIGHTDM_DROPIN: &str = "/etc/lightdm/lightdm.conf.d/90-rabita.conf";

const MKHOMEDIR_PROFILE_CONTENT: &str = "\
Name: Create home directory on first login (Rabita)
Default: yes
Priority: 900
Session-Type: Additional
Session:
\trequired\t\t\tpam_mkhomedir.so umask=0077 skel=/etc/skel
";

const GDM_DROPIN_CONTENT: &str = "\
# Installed by Rabita. Lets staff type their work username at the login screen.
[org/gnome/login-screen]
disable-user-list=true
";

const LIGHTDM_DROPIN_CONTENT: &str = "\
# Installed by Rabita. Lets staff type their work username at the login screen.
[Seat:*]
greeter-show-manual-login=true
greeter-hide-users=false
";

fn noninteractive() -> Vec<(String, String)> {
    vec![("DEBIAN_FRONTEND".to_owned(), "noninteractive".to_owned())]
}

impl Debian {
    fn mkhomedir_profile(facts: &SystemFacts) -> &'static str {
        if facts.has_stock_mkhomedir_profile {
            "mkhomedir"
        } else {
            RABITA_MKHOMEDIR_PROFILE
        }
    }
}

impl Backend for Debian {
    fn family(&self) -> DistroFamily {
        DistroFamily::Debian
    }

    fn is_implemented(&self) -> bool {
        true
    }

    fn packages(&self) -> Vec<&'static str> {
        vec![
            "realmd",
            "sssd",
            "sssd-tools",
            "adcli",
            "krb5-user",
            "libnss-sss",
            "libpam-sss",
            "samba-common-bin",
            "cifs-utils",
            "keyutils",
            "cups",
        ]
    }

    fn install_packages(&self, packages: &[&str], realm: &str) -> Vec<Action> {
        let preseed = format!(
            "krb5-config krb5-config/default_realm string {realm}\n\
             krb5-config krb5-config/add_servers boolean false\n"
        );
        let mut install_args = vec!["install", "-y", "-q"];
        install_args.extend_from_slice(packages);
        vec![
            Action::Run {
                description: "Pre-answer Kerberos setup questions".to_owned(),
                program: "debconf-set-selections".to_owned(),
                args: Vec::new(),
                env: noninteractive(),
                stdin: Some(preseed),
                required: false,
            },
            Action::Run {
                description: "Refresh the package list".to_owned(),
                program: "apt-get".to_owned(),
                args: vec!["update".to_owned(), "-q".to_owned()],
                env: noninteractive(),
                stdin: None,
                required: false,
            },
            Action::Run {
                description: "Install domain login packages".to_owned(),
                program: "apt-get".to_owned(),
                args: install_args.into_iter().map(str::to_owned).collect(),
                env: noninteractive(),
                stdin: None,
                required: true,
            },
        ]
    }

    fn enable_login_integration(&self, facts: &SystemFacts) -> Vec<Action> {
        let mut actions = Vec::new();
        if facts.mkhomedir_already_enabled {
            return actions;
        }
        let profile = Self::mkhomedir_profile(facts);
        if profile == RABITA_MKHOMEDIR_PROFILE {
            actions.push(Action::write_file(
                "Add a login rule that creates home folders",
                RABITA_MKHOMEDIR_PATH,
                MKHOMEDIR_PROFILE_CONTENT,
            ));
        }
        actions.push(Action::Run {
            description: "Turn on domain sign-in and home folder creation".to_owned(),
            program: "pam-auth-update".to_owned(),
            args: vec![
                "--package".to_owned(),
                "--enable".to_owned(),
                "sss".to_owned(),
                "--enable".to_owned(),
                profile.to_owned(),
            ],
            env: noninteractive(),
            stdin: None,
            required: true,
        });
        actions
    }

    fn disable_login_integration(&self, facts: &SystemFacts) -> Vec<Action> {
        if facts.mkhomedir_already_enabled {
            return Vec::new();
        }
        let profile = Self::mkhomedir_profile(facts);
        let mut actions = vec![Action::Run {
            description: "Turn off home folder creation rule".to_owned(),
            program: "pam-auth-update".to_owned(),
            args: vec![
                "--package".to_owned(),
                "--remove".to_owned(),
                profile.to_owned(),
            ],
            env: noninteractive(),
            stdin: None,
            required: false,
        }];
        if profile == RABITA_MKHOMEDIR_PROFILE {
            actions.push(Action::remove_file(
                "Remove Rabita's home folder login rule",
                RABITA_MKHOMEDIR_PATH,
            ));
        }
        actions
    }

    fn enable_manual_login(&self, facts: &SystemFacts) -> Vec<Action> {
        match facts.display_manager {
            Some(DisplayManager::Gdm) => {
                let mut actions = Vec::new();
                if !facts.gdm_dconf_profile_exists {
                    let mut profile = String::from("user-db:user\nsystem-db:gdm\n");
                    if let Some(db) = &facts.gdm_defaults_db {
                        profile.push_str("file-db:");
                        profile.push_str(&db.display().to_string());
                        profile.push('\n');
                    }
                    actions.push(Action::write_file(
                        "Create login screen settings profile",
                        GDM_DCONF_PROFILE,
                        &profile,
                    ));
                }
                actions.push(Action::write_file(
                    "Show a username box on the login screen",
                    GDM_DCONF_DROPIN,
                    GDM_DROPIN_CONTENT,
                ));
                actions.push(Action::run(
                    "Apply login screen settings",
                    "dconf",
                    &["update"],
                ));
                actions
            }
            Some(DisplayManager::LightDm) => vec![Action::write_file(
                "Show a username box on the login screen",
                LIGHTDM_DROPIN,
                LIGHTDM_DROPIN_CONTENT,
            )],
            // SDDM shows an "Other" entry by default in most themes; nothing to do yet.
            Some(DisplayManager::Sddm | DisplayManager::Unknown) | None => Vec::new(),
        }
    }

    fn disable_manual_login(&self, facts: &SystemFacts) -> Vec<Action> {
        match facts.display_manager {
            Some(DisplayManager::Gdm) => vec![
                Action::remove_file("Remove login screen username box setting", GDM_DCONF_DROPIN),
                Action::run_optional("Apply login screen settings", "dconf", &["update"]),
            ],
            Some(DisplayManager::LightDm) => vec![Action::remove_file(
                "Remove login screen username box setting",
                LIGHTDM_DROPIN,
            )],
            Some(DisplayManager::Sddm | DisplayManager::Unknown) | None => not_implemented(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn argv(action: &Action) -> (String, Vec<String>) {
        match action {
            Action::Run { program, args, .. } => (program.clone(), args.clone()),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn install_is_noninteractive_and_preseeds_realm() {
        let actions = Debian.install_packages(&["realmd", "sssd"], "LAB.EXAMPLE.TEST");
        assert_eq!(actions.len(), 3);
        match &actions[0] {
            Action::Run {
                program,
                stdin: Some(stdin),
                env,
                ..
            } => {
                assert_eq!(program, "debconf-set-selections");
                assert!(stdin.contains("default_realm string LAB.EXAMPLE.TEST"));
                assert!(env
                    .iter()
                    .any(|(k, v)| k == "DEBIAN_FRONTEND" && v == "noninteractive"));
            }
            other => panic!("unexpected {other:?}"),
        }
        let (program, args) = argv(&actions[2]);
        assert_eq!(program, "apt-get");
        assert_eq!(args, ["install", "-y", "-q", "realmd", "sssd"]);
    }

    #[test]
    fn login_integration_uses_stock_profile_when_present() {
        let facts = SystemFacts {
            has_stock_mkhomedir_profile: true,
            ..SystemFacts::default()
        };
        let actions = Debian.enable_login_integration(&facts);
        assert_eq!(actions.len(), 1);
        let (program, args) = argv(&actions[0]);
        assert_eq!(program, "pam-auth-update");
        assert_eq!(
            args,
            ["--package", "--enable", "sss", "--enable", "mkhomedir"]
        );
    }

    #[test]
    fn login_integration_installs_own_profile_when_missing() {
        let facts = SystemFacts::default();
        let actions = Debian.enable_login_integration(&facts);
        assert_eq!(actions.len(), 2);
        assert!(
            matches!(&actions[0], Action::WriteFile { path, .. } if path == &PathBuf::from(RABITA_MKHOMEDIR_PATH))
        );
        let (_, args) = argv(&actions[1]);
        assert!(args.contains(&RABITA_MKHOMEDIR_PROFILE.to_owned()));

        let undo = Debian.disable_login_integration(&facts);
        assert!(
            matches!(&undo[1], Action::RemoveFile { path, .. } if path == &PathBuf::from(RABITA_MKHOMEDIR_PATH))
        );
    }

    #[test]
    fn login_integration_is_skipped_when_already_enabled() {
        let facts = SystemFacts {
            mkhomedir_already_enabled: true,
            ..SystemFacts::default()
        };
        assert_eq!(
            Debian.enable_login_integration(&facts),
            Vec::<Action>::new()
        );
        assert_eq!(
            Debian.disable_login_integration(&facts),
            Vec::<Action>::new()
        );
    }

    #[test]
    fn gdm_manual_login_creates_profile_only_when_missing() {
        let facts = SystemFacts {
            display_manager: Some(DisplayManager::Gdm),
            gdm_dconf_profile_exists: false,
            gdm_defaults_db: Some(PathBuf::from("/var/lib/gdm3/greeter-dconf-defaults")),
            ..SystemFacts::default()
        };
        let actions = Debian.enable_manual_login(&facts);
        assert_eq!(actions.len(), 3);
        match &actions[0] {
            Action::WriteFile { path, content, .. } => {
                assert_eq!(path, &PathBuf::from(GDM_DCONF_PROFILE));
                assert!(content.contains("system-db:gdm"));
                assert!(content.contains("file-db:/var/lib/gdm3/greeter-dconf-defaults"));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(
            argv(&actions[2]),
            ("dconf".to_owned(), vec!["update".to_owned()])
        );

        let facts = SystemFacts {
            gdm_dconf_profile_exists: true,
            ..facts
        };
        let actions = Debian.enable_manual_login(&facts);
        assert_eq!(actions.len(), 2);
        assert!(
            matches!(&actions[0], Action::WriteFile { path, .. } if path == &PathBuf::from(GDM_DCONF_DROPIN))
        );
    }

    #[test]
    fn lightdm_manual_login_is_a_dropin() {
        let facts = SystemFacts {
            display_manager: Some(DisplayManager::LightDm),
            ..SystemFacts::default()
        };
        let actions = Debian.enable_manual_login(&facts);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::WriteFile { path, content, .. } => {
                assert_eq!(path, &PathBuf::from(LIGHTDM_DROPIN));
                assert!(content.contains("greeter-show-manual-login=true"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn no_secrets_in_argv() {
        for action in Debian.install_packages(&["realmd"], "R") {
            if let Action::Run { args, .. } = action {
                assert!(args.iter().all(|a| !a.contains("password")));
            }
        }
    }
}
