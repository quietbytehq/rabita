// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! Explains why the helper does not answer, in words for the wizard.
//!
//! Only reads files any user may read; nothing here needs root. All paths
//! hang off `root` so tests can use a temporary directory.

use std::path::Path;

/// The systemd unit the package installs, and the name a local copy in
/// `/etc` would take to replace it.
const UNIT: &str = "rabita-helper.service";
/// Where the package installs the helper.
const PACKAGED_HELPER: &str = "/usr/libexec/rabita-helper";
/// D-Bus activation files, in the order the system bus reads them.
const LOCAL_ACTIVATION: &str =
    "usr/local/share/dbus-1/system-services/org.quietbyte.Rabita1.service";
const ACTIVATION: &str = "usr/share/dbus-1/system-services/org.quietbyte.Rabita1.service";

/// What the technician can do, after the reasons.
pub const REMEDY: &str = "Restart the computer and open Rabita Setup again. If that does not help, remove Rabita in App Center and install it again: installing moves leftover test files out of the way.";

fn is_dev_copy(text: &str) -> bool {
    text.contains("scripts/dev-install.sh")
        || text.contains("/usr/local/libexec/rabita-helper")
        || text.contains("rabita-helper-dev.service")
}

fn read(root: &Path, rel: &str) -> Option<String> {
    std::fs::read_to_string(root.join(rel)).ok()
}

/// The program an activation file starts (`Exec=` line).
fn exec_of(activation: &str) -> Option<&str> {
    activation
        .lines()
        .find_map(|l| l.trim().strip_prefix("Exec="))
        .and_then(|rest| rest.split_whitespace().next())
}

/// Plain-language reasons the helper may be missing, stuck or the wrong
/// copy. `owner_cmdline` is the command line of the process that owns the
/// helper's bus name, if any (NUL-separated, as in `/proc/<pid>/cmdline`).
#[must_use]
pub fn reasons(root: &Path, owner_cmdline: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();

    let etc_unit = root.join("etc/systemd/system").join(UNIT);
    if let Ok(target) = std::fs::read_link(&etc_unit) {
        if target == Path::new("/dev/null") {
            out.push(
                "Rabita's background service is switched off (masked) on this computer.".to_owned(),
            );
        }
    }
    if out.is_empty() {
        if let Some(text) = read(root, &format!("etc/systemd/system/{UNIT}")) {
            out.push(if is_dev_copy(&text) {
                "A leftover test installation of Rabita (/etc/systemd/system/rabita-helper.service) replaces the installed background service.".to_owned()
            } else {
                "A local file, /etc/systemd/system/rabita-helper.service, replaces the installed background service's settings.".to_owned()
            });
        }
    }

    let dropins = root.join(format!("etc/systemd/system/{UNIT}.d"));
    if let Ok(entries) = std::fs::read_dir(&dropins) {
        if entries
            .flatten()
            .any(|e| e.path().extension().is_some_and(|x| x == "conf"))
        {
            out.push("Local settings in /etc/systemd/system/rabita-helper.service.d change how the background service starts.".to_owned());
        }
    }

    if read(root, LOCAL_ACTIVATION).is_some() {
        out.push("A file in /usr/local/share/dbus-1/system-services starts a different copy of the background service.".to_owned());
    }

    if let Some(text) = read(root, ACTIVATION) {
        if is_dev_copy(&text) {
            out.push("A test (development) version of Rabita's background service is installed instead of the packaged one.".to_owned());
        } else if let Some(exec) = exec_of(&text) {
            let program = root.join(exec.trim_start_matches('/'));
            if !program.exists() {
                out.push(format!(
                    "The background service program ({exec}) is missing."
                ));
            }
        }
    }

    if let Some(cmd) = owner_cmdline {
        let program = cmd.split('\0').next().unwrap_or_default();
        if !program.is_empty() && program != PACKAGED_HELPER {
            out.push(format!(
                "A different copy of the background service is running ({program})."
            ));
        }
    }
    out
}

/// `message`, then the reasons found, then what to do.
#[must_use]
pub fn explain(message: &str, reasons: &[String]) -> String {
    let mut text = message.trim_end().to_owned();
    for r in reasons {
        text.push(' ');
        text.push_str(r);
    }
    text.push(' ');
    text.push_str(REMEDY);
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn root() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    const PACKAGED: &str =
        "[D-BUS Service]\nName=org.quietbyte.Rabita1\nExec=/usr/libexec/rabita-helper\nUser=root\nSystemdService=rabita-helper.service\n";

    #[test]
    fn a_clean_install_has_nothing_to_report() {
        let r = root();
        write(r.path(), ACTIVATION, PACKAGED);
        write(r.path(), "usr/libexec/rabita-helper", "");
        assert_eq!(
            reasons(r.path(), Some("/usr/libexec/rabita-helper\0")),
            Vec::<String>::new()
        );
        assert_eq!(reasons(r.path(), None), Vec::<String>::new());
    }

    #[test]
    fn finds_the_old_dev_install_override() {
        let r = root();
        write(r.path(), ACTIVATION, PACKAGED);
        write(r.path(), "usr/libexec/rabita-helper", "");
        write(
            r.path(),
            "etc/systemd/system/rabita-helper.service",
            "[Service]\nType=dbus\nExecStart=/usr/local/libexec/rabita-helper\n",
        );
        let found = reasons(r.path(), Some("/usr/local/libexec/rabita-helper\0"));
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].contains("leftover test installation"));
        assert!(found[1].contains("(/usr/local/libexec/rabita-helper)"));
    }

    #[test]
    fn tells_a_local_override_from_a_test_install() {
        let r = root();
        write(
            r.path(),
            "etc/systemd/system/rabita-helper.service",
            "[Service]\nExecStart=/opt/x\n",
        );
        write(
            r.path(),
            "etc/systemd/system/rabita-helper.service.d/10-x.conf",
            "[Service]\n",
        );
        write(r.path(), LOCAL_ACTIVATION, PACKAGED);
        let found = reasons(r.path(), None);
        assert!(found[0].starts_with("A local file"), "{found:?}");
        assert!(found[1].contains("rabita-helper.service.d"));
        assert!(found[2].contains("/usr/local/share/dbus-1/system-services"));
    }

    #[test]
    fn finds_a_masked_service() {
        let r = root();
        fs::create_dir_all(r.path().join("etc/systemd/system")).unwrap();
        std::os::unix::fs::symlink(
            "/dev/null",
            r.path().join("etc/systemd/system/rabita-helper.service"),
        )
        .unwrap();
        let found = reasons(r.path(), None);
        assert_eq!(found.len(), 1);
        assert!(found[0].contains("masked"));
    }

    #[test]
    fn finds_a_dev_build_or_a_missing_program() {
        let r = root();
        write(
            r.path(),
            ACTIVATION,
            "Exec=/usr/local/libexec/rabita-helper\nSystemdService=rabita-helper-dev.service\n",
        );
        assert!(reasons(r.path(), None)[0].contains("development"));
        write(r.path(), ACTIVATION, PACKAGED);
        assert_eq!(
            reasons(r.path(), None),
            ["The background service program (/usr/libexec/rabita-helper) is missing."]
        );
    }

    #[test]
    fn explanation_ends_with_what_to_do() {
        let text = explain("It did not answer.", &["Reason one.".to_owned()]);
        assert!(text.starts_with("It did not answer. Reason one. Restart the computer"));
        assert_eq!(explain("X.", &[]), format!("X. {REMEDY}"));
    }
}
