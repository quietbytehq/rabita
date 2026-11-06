// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! Backend for Fedora, RHEL, Rocky and Alma Linux (dnf, authselect).
//!
//! Not implemented yet (Milestone 7). Until then the wizard reports
//! "Not supported yet" for this family and changes nothing.

use crate::{not_implemented, Action, Backend, DistroFamily, SystemFacts};

/// Fedora, RHEL, Rocky and Alma Linux backend (placeholder).
#[derive(Debug, Default, Clone, Copy)]
pub struct Fedora;

impl Backend for Fedora {
    fn family(&self) -> DistroFamily {
        DistroFamily::Fedora
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
