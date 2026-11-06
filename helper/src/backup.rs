// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! Remembering what Rabita changed so that "Leave domain" can undo it.
//!
//! The first time a file is touched, a copy of its original content (or a
//! note that it did not exist) goes to `/var/lib/rabita/backups/<id>/`. The
//! state file lists every backup, every file Rabita created from scratch and
//! the join details. Restoring walks that list in reverse.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::paths::Paths;
use crate::types::JoinOptions;

/// One backed-up file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupEntry {
    /// The system path that was changed.
    pub original: PathBuf,
    /// Where the pre-change copy lives (when `existed`).
    pub backup: Option<PathBuf>,
    /// Whether the file existed before Rabita touched it.
    pub existed: bool,
    /// Original permission bits (when `existed`).
    pub mode: Option<u32>,
}

/// Everything Rabita needs to remember between a join and a leave.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// Domain joined, when the join got that far.
    pub domain: Option<String>,
    /// Kerberos realm.
    pub realm: Option<String>,
    /// Computer account name used.
    pub computer_name: Option<String>,
    /// Whether `realm join` succeeded.
    pub joined: bool,
    /// Unix timestamp of the join.
    pub joined_at: Option<u64>,
    /// Domain controller the join talked to.
    pub domain_controller: Option<String>,
    /// User looked up during verification, if any.
    pub verify_user: Option<String>,
    /// Options chosen in the wizard.
    pub options: JoinOptions,
    /// Backups, in the order they were taken.
    pub backups: Vec<BackupEntry>,
    /// Whether home-directory creation was already enabled before the join.
    pub mkhomedir_was_enabled: bool,
}

/// Errors from the store.
#[derive(Debug)]
pub enum StoreError {
    /// File system problem.
    Io(PathBuf, io::Error),
    /// The state file is unreadable.
    Corrupt(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(p, e) => write!(f, "{}: {e}", p.display()),
            Self::Corrupt(e) => write!(f, "state file is damaged: {e}"),
        }
    }
}

impl std::error::Error for StoreError {}

/// Reads, writes and applies [`State`].
#[derive(Debug, Clone)]
pub struct Store {
    paths: Paths,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn io_err(p: &Path) -> impl FnOnce(io::Error) -> StoreError + '_ {
    move |e| StoreError::Io(p.to_path_buf(), e)
}

impl Store {
    /// A store for the given layout.
    #[must_use]
    pub fn new(paths: Paths) -> Self {
        Self { paths }
    }

    /// Loads the state, or an empty state if none was saved.
    pub fn load(&self) -> Result<State, StoreError> {
        let path = self.paths.state_file();
        match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| StoreError::Corrupt(e.to_string())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(State::default()),
            Err(e) => Err(StoreError::Io(path, e)),
        }
    }

    /// Whether a state file exists at all.
    #[must_use]
    pub fn exists(&self) -> bool {
        self.paths.state_file().is_file()
    }

    /// Saves the state (0600, root only).
    pub fn save(&self, state: &State) -> Result<(), StoreError> {
        let dir = self.paths.state_dir();
        fs::create_dir_all(&dir).map_err(io_err(&dir))?;
        let path = self.paths.state_file();
        let tmp = path.with_extension("json.tmp");
        let text =
            serde_json::to_string_pretty(state).map_err(|e| StoreError::Corrupt(e.to_string()))?;
        fs::write(&tmp, text).map_err(io_err(&tmp))?;
        set_mode(&tmp, 0o600).map_err(io_err(&tmp))?;
        fs::rename(&tmp, &path).map_err(io_err(&path))?;
        Ok(())
    }

    /// Removes the state file, keeping a copy as `last-leave.json` for
    /// troubleshooting.
    pub fn clear(&self) -> Result<(), StoreError> {
        let path = self.paths.state_file();
        if path.is_file() {
            let keep = self.paths.state_dir().join("last-leave.json");
            fs::rename(&path, &keep).map_err(io_err(&path))?;
        }
        Ok(())
    }

    /// Records a backup of `original` (a real system path) unless one was
    /// already taken, then returns the updated state. Call this before every
    /// edit.
    pub fn ensure_backup(&self, state: &mut State, original: &Path) -> Result<(), StoreError> {
        if state.backups.iter().any(|b| b.original == original) {
            return Ok(());
        }
        let real = self.paths.resolve(original);
        let entry = if real.is_file() {
            let id = state.joined_at.unwrap_or_else(now_secs);
            let dir = self.paths.backup_dir().join(id.to_string());
            fs::create_dir_all(&dir).map_err(io_err(&dir))?;
            let name = original
                .to_string_lossy()
                .trim_start_matches('/')
                .replace('/', "__");
            let dest = dir.join(name);
            fs::copy(&real, &dest).map_err(io_err(&real))?;
            let mode = file_mode(&real).map_err(io_err(&real))?;
            set_mode(&dest, 0o600).map_err(io_err(&dest))?;
            BackupEntry {
                original: original.to_path_buf(),
                backup: Some(dest),
                existed: true,
                mode: Some(mode),
            }
        } else {
            BackupEntry {
                original: original.to_path_buf(),
                backup: None,
                existed: false,
                mode: None,
            }
        };
        state.backups.push(entry);
        self.save(state)
    }

    /// Writes `content` to `original` (a real system path), backing it up
    /// first and creating parent directories.
    pub fn write_file(
        &self,
        state: &mut State,
        original: &Path,
        content: &str,
        mode: u32,
    ) -> Result<(), StoreError> {
        self.ensure_backup(state, original)?;
        let real = self.paths.resolve(original);
        if let Some(parent) = real.parent() {
            fs::create_dir_all(parent).map_err(io_err(parent))?;
        }
        fs::write(&real, content).map_err(io_err(&real))?;
        set_mode(&real, mode).map_err(io_err(&real))?;
        Ok(())
    }

    /// Puts every backed-up file back, newest change first. Files that did
    /// not exist before are removed. Returns plain-language notes of what
    /// was done.
    pub fn restore_all(&self, state: &State) -> Result<Vec<String>, StoreError> {
        let mut notes = Vec::new();
        for entry in state.backups.iter().rev() {
            let real = self.paths.resolve(&entry.original);
            if entry.existed {
                if let Some(backup) = &entry.backup {
                    if let Some(parent) = real.parent() {
                        fs::create_dir_all(parent).map_err(io_err(parent))?;
                    }
                    fs::copy(backup, &real).map_err(io_err(backup))?;
                    if let Some(mode) = entry.mode {
                        set_mode(&real, mode).map_err(io_err(&real))?;
                    }
                    notes.push(format!("Restored {}", entry.original.display()));
                }
            } else {
                match fs::remove_file(&real) {
                    Ok(()) => notes.push(format!("Removed {}", entry.original.display())),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(StoreError::Io(real, e)),
                }
            }
        }
        Ok(notes)
    }
}

fn file_mode(p: &Path) -> io::Result<u32> {
    use std::os::unix::fs::PermissionsExt;
    Ok(fs::metadata(p)?.permissions().mode() & 0o7777)
}

fn set_mode(p: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(mode))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(Paths::under(dir.path()));
        (dir, store)
    }

    #[test]
    fn load_without_state_is_empty() {
        let (_d, store) = store();
        assert_eq!(store.load().unwrap(), State::default());
        assert!(!store.exists());
    }

    #[test]
    fn save_and_load_round_trip() {
        let (_d, store) = store();
        let state = State {
            domain: Some("lab.test".into()),
            joined: true,
            ..State::default()
        };
        store.save(&state).unwrap();
        assert!(store.exists());
        assert_eq!(store.load().unwrap(), state);
    }

    #[test]
    fn backs_up_existing_file_once_and_restores_it() {
        let (dir, store) = store();
        let paths = Paths::under(dir.path());
        let conf = paths.sssd_conf();
        fs::create_dir_all(conf.parent().unwrap()).unwrap();
        fs::write(&conf, "original\n").unwrap();
        set_mode(&conf, 0o600).unwrap();

        let mut state = State::default();
        store
            .write_file(
                &mut state,
                Path::new("/etc/sssd/sssd.conf"),
                "changed\n",
                0o644,
            )
            .unwrap();
        store
            .write_file(
                &mut state,
                Path::new("/etc/sssd/sssd.conf"),
                "changed again\n",
                0o644,
            )
            .unwrap();
        assert_eq!(state.backups.len(), 1, "second write must not add a backup");
        assert_eq!(fs::read_to_string(&conf).unwrap(), "changed again\n");
        assert_eq!(
            fs::read_to_string(state.backups[0].backup.as_ref().unwrap()).unwrap(),
            "original\n"
        );

        let notes = store.restore_all(&state).unwrap();
        assert_eq!(fs::read_to_string(&conf).unwrap(), "original\n");
        assert_eq!(file_mode(&conf).unwrap(), 0o600);
        assert_eq!(notes, ["Restored /etc/sssd/sssd.conf"]);
    }

    #[test]
    fn removes_files_that_did_not_exist_before() {
        let (dir, store) = store();
        let mut state = State::default();
        let p = Path::new("/etc/lightdm/lightdm.conf.d/90-rabita.conf");
        store.write_file(&mut state, p, "x\n", 0o644).unwrap();
        let real = Paths::under(dir.path()).resolve(p);
        assert!(real.is_file());
        assert!(!state.backups[0].existed);

        let notes = store.restore_all(&state).unwrap();
        assert!(!real.exists());
        assert_eq!(
            notes,
            ["Removed /etc/lightdm/lightdm.conf.d/90-rabita.conf"]
        );
        // Restoring twice is harmless.
        store.restore_all(&state).unwrap();
    }

    #[test]
    fn state_file_is_private_and_clear_keeps_a_copy() {
        let (dir, store) = store();
        store.save(&State::default()).unwrap();
        let paths = Paths::under(dir.path());
        assert_eq!(file_mode(&paths.state_file()).unwrap(), 0o600);
        store.clear().unwrap();
        assert!(!store.exists());
        assert!(paths.state_dir().join("last-leave.json").is_file());
    }
}
