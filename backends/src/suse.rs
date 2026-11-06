// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! Backend for openSUSE Leap and Tumbleweed (zypper, pam-config).
//!
//! Not implemented yet (Milestone 7). Until then the wizard reports
//! "Not supported yet" for this family and changes nothing.

use crate::{not_implemented, Action, Backend, DistroFamily, SystemFacts};

/// openSUSE Leap and Tumbleweed backend (placeholder).
#[derive(Debug, Default, Clone, Copy)]
pub struct Suse;

impl Backend for Suse {
    fn family(&self) -> DistroFamily {
        DistroFamily::Suse
    }

    fn is_implemented(&self) -> bool {
        false
    }

    fn packages(&self) -> Vec<&'static str> {
        Vec::new()
    }

    fn install_packages(&self, _packages: &[&str], _realm: &str) -> Vec<Action> {
        not_implemented()
    }

    fn enable_login_integration(&self, _facts: &SystemFacts) -> Vec<Action> {
        not_implemented()
    }

    fn disable_login_integration(&self, _facts: &SystemFacts) -> Vec<Action> {
        not_implemented()
    }

    fn enable_manual_login(&self, _facts: &SystemFacts) -> Vec<Action> {
        not_implemented()
    }

    fn disable_manual_login(&self, _facts: &SystemFacts) -> Vec<Action> {
        not_implemented()
    }
}
