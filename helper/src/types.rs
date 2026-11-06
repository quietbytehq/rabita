// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! Data exchanged with the wizard and the CLI, as JSON strings over D-Bus.
//!
//! JSON keeps the D-Bus interface stable while these structures grow, and
//! the TypeScript side of the wizard can consume it directly.

use rabita_backends::{DisplayManager, DistroFamily};
use serde::{Deserialize, Serialize};

/// Result of `DetectSystem`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemInfo {
    /// `ID` from `/etc/os-release` (e.g. `ubuntu`).
    pub distro_id: String,
    /// `PRETTY_NAME` from `/etc/os-release`.
    pub distro_name: String,
    /// `VERSION_ID` from `/etc/os-release`.
    pub version_id: String,
    /// Family Rabita mapped the distribution to, if any.
    pub family: Option<DistroFamily>,
    /// Whether Rabita can set this machine up.
    pub supported: bool,
    /// Plain-language reason when `supported` is false.
    pub unsupported_reason: Option<String>,
    /// Login screen in use.
    pub display_manager: DisplayManager,
    /// This computer's host name.
    pub hostname: String,
    /// Domain this machine is already joined to, if any.
    pub joined_domain: Option<String>,
    /// Whether `realm` (realmd) is installed.
    pub realmd_installed: bool,
    /// Whether `sssd` is installed.
    pub sssd_installed: bool,
}

/// One domain controller found by `DiscoverDomain`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DomainController {
    /// Host name from the DNS SRV record.
    pub host: String,
    /// LDAP port from the SRV record (normally 389).
    pub port: u16,
    /// SRV priority (lower is preferred).
    pub priority: u16,
    /// SRV weight.
    pub weight: u16,
    /// Whether a TCP connection to the LDAP port succeeded.
    pub reachable: bool,
    /// Clock difference (DC minus this machine) in seconds, if measured.
    pub clock_offset_secs: Option<f64>,
}

/// Result of `DiscoverDomain`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryResult {
    /// Domain name as entered, lower-cased.
    pub domain: String,
    /// Kerberos realm (upper-cased domain).
    pub realm: String,
    /// Whether at least one DC answered.
    pub found: bool,
    /// Domain controllers, best first.
    pub controllers: Vec<DomainController>,
    /// Plain-language problem description when `found` is false.
    pub problem: Option<String>,
}

/// Options the wizard offers on the "Choose options" screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct JoinOptions {
    /// Let people type a username at the login screen.
    pub manual_login: bool,
    /// Create a home folder on first login.
    pub create_home: bool,
    /// Keep cached passwords so people can log in offline.
    pub offline_login: bool,
}

impl Default for JoinOptions {
    fn default() -> Self {
        Self {
            manual_login: true,
            create_home: true,
            offline_login: true,
        }
    }
}

/// Request body for `Join` (JSON). The password travels separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinRequest {
    /// Domain to join, e.g. `lab.quietbyte.test`.
    pub domain: String,
    /// Account allowed to join computers, e.g. `joiner` or `joiner@LAB...`.
    pub admin_user: String,
    /// Organizational unit for the computer object, if not the default.
    #[serde(default)]
    pub computer_ou: Option<String>,
    /// Computer account name, if not the host name.
    #[serde(default)]
    pub computer_name: Option<String>,
    /// Domain user to look up at the end to prove it worked.
    #[serde(default)]
    pub verify_user: Option<String>,
    /// Wizard options.
    #[serde(default)]
    pub options: JoinOptions,
}

/// Request body for `Leave` (JSON). The password travels separately.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LeaveRequest {
    /// Also delete the computer account from the domain. Needs `admin_user`
    /// and a password.
    pub remove_computer_account: bool,
    /// Account used to delete the computer account.
    pub admin_user: Option<String>,
}

/// Progress state of a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StepState {
    /// Not started yet.
    Pending,
    /// In progress.
    Running,
    /// Finished successfully.
    Done,
    /// Finished with an error; the job stops.
    Failed,
    /// Not needed on this machine.
    Skipped,
}

impl StepState {
    /// Lower-case name used on D-Bus.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

/// Outcome of one step, included in the final result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepOutcome {
    /// Machine-readable step id (e.g. `packages`).
    pub id: String,
    /// Plain-language label.
    pub label: String,
    /// Final state.
    pub state: StepState,
    /// Plain-language message (what happened, or what went wrong).
    pub message: String,
}

/// Final result of a `Join`, `Leave` or `ApplyOptions` job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobResult {
    /// Whether every required step succeeded.
    pub ok: bool,
    /// Plain-language summary.
    pub summary: String,
    /// Every step, in order.
    pub steps: Vec<StepOutcome>,
}

/// One health check in `Status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthCheck {
    /// Machine-readable id.
    pub id: String,
    /// Plain-language label.
    pub label: String,
    /// `Some(true)` pass, `Some(false)` fail, `None` not applicable.
    pub ok: Option<bool>,
    /// Plain-language detail.
    pub detail: String,
}

/// Result of `Status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusReport {
    /// Whether this computer is a member of a domain.
    pub joined: bool,
    /// Domain name, when joined.
    pub domain: Option<String>,
    /// Kerberos realm, when joined.
    pub realm: Option<String>,
    /// Computer account name, when known.
    pub computer_name: Option<String>,
    /// Domain controller the machine talks to, when known.
    pub domain_controller: Option<String>,
    /// When Rabita joined (RFC 3339), when known.
    pub joined_at: Option<String>,
    /// Health checks.
    pub checks: Vec<HealthCheck>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_request_defaults_are_sensible() {
        let req: JoinRequest =
            serde_json::from_str(r#"{"domain":"lab.test","admin_user":"joiner"}"#).unwrap();
        assert_eq!(req.computer_ou, None);
        assert!(req.options.manual_login);
        assert!(req.options.create_home);
        assert!(req.options.offline_login);
    }

    #[test]
    fn job_result_round_trips() {
        let result = JobResult {
            ok: true,
            summary: "Joined".into(),
            steps: vec![StepOutcome {
                id: "join".into(),
                label: "Join".into(),
                state: StepState::Done,
                message: "ok".into(),
            }],
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"state\":\"done\""));
        let back: JobResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back, result);
    }
}
