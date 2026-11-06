// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! Running system tools as child processes.
//!
//! Everything that touches `realm`, `apt-get`, `systemctl` and friends goes
//! through the [`Runner`] trait, so unit tests can use [`FakeRunner`] and the
//! real system is never changed by `cargo test`.
//!
//! Secrets are only ever passed on standard input, and standard input is
//! never logged.

use std::collections::HashMap;
use std::fmt;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use zeroize::Zeroizing;

/// A command to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    /// Program name, looked up on `PATH`.
    pub program: String,
    /// Arguments. Never put secrets here: they show up in `ps`.
    pub args: Vec<String>,
    /// Extra environment variables.
    pub env: Vec<(String, String)>,
    /// Data written to the child's standard input (may hold a secret).
    pub stdin: Option<Zeroizing<String>>,
    /// Give up after this long.
    pub timeout: Duration,
}

impl CommandSpec {
    /// A command with a default two-minute timeout.
    #[must_use]
    pub fn new(program: &str, args: &[&str]) -> Self {
        Self {
            program: program.to_owned(),
            args: args.iter().map(|s| (*s).to_owned()).collect(),
            env: Vec::new(),
            stdin: None,
            timeout: Duration::from_secs(120),
        }
    }

    /// Sets the timeout.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Adds an environment variable.
    #[must_use]
    pub fn env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_owned(), value.to_owned()));
        self
    }

    /// Sets standard input (wiped from memory when dropped).
    #[must_use]
    pub fn stdin(mut self, data: Zeroizing<String>) -> Self {
        self.stdin = Some(data);
        self
    }

    /// `program arg1 arg2…` for logs. Standard input is never included.
    #[must_use]
    pub fn display(&self) -> String {
        let mut s = self.program.clone();
        for a in &self.args {
            s.push(' ');
            s.push_str(a);
        }
        s
    }
}

/// What a finished command produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandOutput {
    /// Exit status code; `None` if killed by a signal or timed out.
    pub code: Option<i32>,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
    /// Whether the command was killed because it exceeded the timeout.
    pub timed_out: bool,
}

impl CommandOutput {
    /// Whether the command exited with status 0.
    #[must_use]
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// Standard output and error combined, trimmed.
    #[must_use]
    pub fn combined(&self) -> String {
        let mut s = self.stdout.trim().to_owned();
        if !self.stderr.trim().is_empty() {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(self.stderr.trim());
        }
        s
    }
}

/// Why a command could not be run at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandError {
    /// The program was not found on `PATH`.
    NotFound(String),
    /// Spawning or talking to the process failed.
    Io(String),
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(p) => write!(f, "'{p}' is not installed"),
            Self::Io(e) => write!(f, "could not run command: {e}"),
        }
    }
}

impl std::error::Error for CommandError {}

/// Runs commands. Implemented by [`SystemRunner`] (real) and
/// [`FakeRunner`] (tests).
pub trait Runner: Send + Sync {
    /// Runs the command to completion and captures its output.
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, CommandError>;

    /// Whether `program` can be found on `PATH`.
    fn which(&self, program: &str) -> bool;
}

/// Runs real processes.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, CommandError> {
        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args)
            .env("LANG", "C.UTF-8")
            .env("LC_ALL", "C.UTF-8")
            .stdin(if spec.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                CommandError::NotFound(spec.program.clone())
            } else {
                CommandError::Io(e.to_string())
            }
        })?;

        if let Some(data) = &spec.stdin {
            if let Some(mut stdin) = child.stdin.take() {
                // A closed pipe (child exited early) is not fatal here; the
                // exit status will tell the story.
                let _ = stdin.write_all(data.as_bytes());
                let _ = stdin.flush();
            }
        }

        let mut stdout_pipe = child.stdout.take();
        let mut stderr_pipe = child.stderr.take();
        let out_thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(p) = stdout_pipe.as_mut() {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        });
        let err_thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(p) = stderr_pipe.as_mut() {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        });

        let started = Instant::now();
        let mut timed_out = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {
                    if started.elapsed() > spec.timeout {
                        timed_out = true;
                        let _ = child.kill();
                        let _ = child.wait();
                        break None;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(CommandError::Io(e.to_string())),
            }
        };
        let stdout = String::from_utf8_lossy(&out_thread.join().unwrap_or_default()).into_owned();
        let stderr = String::from_utf8_lossy(&err_thread.join().unwrap_or_default()).into_owned();
        Ok(CommandOutput {
            code: status.and_then(|s| s.code()),
            stdout,
            stderr,
            timed_out,
        })
    }

    fn which(&self, program: &str) -> bool {
        let Some(path) = std::env::var_os("PATH") else {
            return false;
        };
        std::env::split_paths(&path).any(|dir| dir.join(program).is_file())
            || ["/usr/sbin", "/sbin", "/usr/bin", "/bin"]
                .iter()
                .any(|dir| std::path::Path::new(dir).join(program).is_file())
    }
}

/// Locks a mutex, tolerating poisoning (a panicking test must not hide the
/// calls made before it).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A scripted runner for unit tests. Records every call and answers from a
/// table keyed by the program name (optionally with its first argument).
#[derive(Debug, Default)]
pub struct FakeRunner {
    responses: Mutex<HashMap<String, Vec<CommandOutput>>>,
    installed: Mutex<Vec<String>>,
    calls: Mutex<Vec<CommandSpec>>,
}

impl FakeRunner {
    /// Creates an empty fake runner: every program exists and exits 0 with
    /// no output unless scripted otherwise.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Scripts the next response for `key`, which is either `program` or
    /// `program first-arg`. Responses are consumed in order; the last one
    /// repeats.
    pub fn respond(&self, key: &str, output: CommandOutput) -> &Self {
        lock(&self.responses)
            .entry(key.to_owned())
            .or_default()
            .push(output);
        self
    }

    /// Scripts a successful response with the given stdout.
    pub fn ok(&self, key: &str, stdout: &str) -> &Self {
        self.respond(
            key,
            CommandOutput {
                code: Some(0),
                stdout: stdout.to_owned(),
                ..CommandOutput::default()
            },
        )
    }

    /// Scripts a failing response with the given stderr.
    pub fn fail(&self, key: &str, stderr: &str) -> &Self {
        self.respond(
            key,
            CommandOutput {
                code: Some(1),
                stderr: stderr.to_owned(),
                ..CommandOutput::default()
            },
        )
    }

    /// Restricts `which` to these programs (default: everything exists).
    pub fn installed(&self, programs: &[&str]) -> &Self {
        let mut list = lock(&self.installed);
        list.clear();
        list.extend(programs.iter().map(|p| (*p).to_owned()));
        list.push("__restricted__".to_owned());
        self
    }

    /// Every command that was run, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<CommandSpec> {
        lock(&self.calls).clone()
    }

    /// Display strings of every call, for assertions.
    #[must_use]
    pub fn call_lines(&self) -> Vec<String> {
        self.calls().iter().map(CommandSpec::display).collect()
    }
}

impl Runner for FakeRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, CommandError> {
        lock(&self.calls).push(spec.clone());
        if !self.which(&spec.program) {
            return Err(CommandError::NotFound(spec.program.clone()));
        }
        let mut table = lock(&self.responses);
        let keyed = spec
            .args
            .first()
            .map(|a| format!("{} {a}", spec.program))
            .unwrap_or_default();
        for key in [keyed, spec.program.clone()] {
            if let Some(list) = table.get_mut(&key) {
                if list.len() > 1 {
                    return Ok(list.remove(0));
                }
                if let Some(last) = list.first() {
                    return Ok(last.clone());
                }
            }
        }
        Ok(CommandOutput {
            code: Some(0),
            ..CommandOutput::default()
        })
    }

    fn which(&self, program: &str) -> bool {
        let list = lock(&self.installed);
        if list.iter().any(|p| p == "__restricted__") {
            list.iter().any(|p| p == program)
        } else {
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_never_includes_stdin() {
        let spec = CommandSpec::new("realm", &["join", "lab.test"])
            .stdin(Zeroizing::new("s3cret\n".to_owned()));
        assert_eq!(spec.display(), "realm join lab.test");
    }

    #[test]
    fn system_runner_captures_output_and_status() {
        let out = SystemRunner
            .run(&CommandSpec::new(
                "sh",
                &["-c", "echo hi; echo err >&2; exit 3"],
            ))
            .unwrap();
        assert_eq!(out.code, Some(3));
        assert_eq!(out.stdout.trim(), "hi");
        assert_eq!(out.stderr.trim(), "err");
        assert!(!out.success());
    }

    #[test]
    fn system_runner_feeds_stdin() {
        let out = SystemRunner
            .run(&CommandSpec::new("cat", &[]).stdin(Zeroizing::new("ping".to_owned())))
            .unwrap();
        assert_eq!(out.stdout, "ping");
    }

    #[test]
    fn system_runner_times_out() {
        let out = SystemRunner
            .run(&CommandSpec::new("sleep", &["5"]).timeout(Duration::from_millis(200)))
            .unwrap();
        assert!(out.timed_out);
        assert!(!out.success());
    }

    #[test]
    fn system_runner_reports_missing_program() {
        let err = SystemRunner
            .run(&CommandSpec::new("definitely-not-a-real-program-xyz", &[]))
            .unwrap_err();
        assert!(matches!(err, CommandError::NotFound(_)));
    }

    #[test]
    fn fake_runner_scripts_by_program_and_first_arg() {
        let fake = FakeRunner::new();
        fake.ok("realm list", "lab.test").fail("realm join", "nope");
        assert_eq!(
            fake.run(&CommandSpec::new("realm", &["list"]))
                .unwrap()
                .stdout,
            "lab.test"
        );
        assert!(!fake
            .run(&CommandSpec::new("realm", &["join"]))
            .unwrap()
            .success());
        assert!(fake.run(&CommandSpec::new("ls", &[])).unwrap().success());
        assert_eq!(fake.call_lines(), ["realm list", "realm join", "ls"]);
    }

    #[test]
    fn fake_runner_restricts_installed_programs() {
        let fake = FakeRunner::new();
        fake.installed(&["ls"]);
        assert!(fake.which("ls"));
        assert!(!fake.which("realm"));
        assert!(matches!(
            fake.run(&CommandSpec::new("realm", &[])),
            Err(CommandError::NotFound(_))
        ));
    }
}
