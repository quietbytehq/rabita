// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

// The JSON shapes the helper sends over D-Bus (see docs/dbus-api.md and
// helper/src/types.rs). Field names match the Rust structs exactly.

export type DistroFamily = "debian" | "fedora" | "suse" | "arch";
export type DisplayManager = "gdm" | "sddm" | "light-dm" | "unknown";

export interface SystemInfo {
  distro_id: string;
  distro_name: string;
  version_id: string;
  family: DistroFamily | null;
  supported: boolean;
  unsupported_reason: string | null;
  display_manager: DisplayManager;
  hostname: string;
  joined_domain: string | null;
  realmd_installed: boolean;
  sssd_installed: boolean;
}

export interface DomainController {
  host: string;
  port: number;
  priority: number;
  weight: number;
  reachable: boolean;
  clock_offset_secs: number | null;
  in_client_site: boolean;
  recommended: boolean;
  warning: string | null;
}

export interface DiscoveryResult {
  domain: string;
  realm: string;
  found: boolean;
  controllers: DomainController[];
  problem: string | null;
  client_site: string | null;
  recommended: string | null;
  majority_clock_offset_secs: number | null;
  warnings: string[];
}

export type LoginAccess = { mode: "all" } | { mode: "groups"; groups: string[] };

export interface JoinOptions {
  manual_login: boolean;
  create_home: boolean;
  offline_login: boolean;
  login_access: LoginAccess;
}

/** Sent to `Join`; the password travels separately and is never stored. */
export interface JoinRequest {
  domain: string;
  admin_user: string;
  computer_ou: string | null;
  computer_name: string | null;
  verify_user: string | null;
  options: JoinOptions;
}

export type StepState = "pending" | "running" | "done" | "failed" | "skipped";

export interface StepOutcome {
  id: string;
  label: string;
  state: StepState;
  message: string;
}

export interface JobResult {
  ok: boolean;
  summary: string;
  steps: StepOutcome[];
  joined_not_finished: boolean;
  remaining_steps: string[];
}

export interface HealthCheck {
  id: string;
  label: string;
  ok: boolean | null;
  detail: string;
  warning: boolean;
}

export interface StatusReport {
  joined: boolean;
  domain: string | null;
  realm: string | null;
  computer_name: string | null;
  domain_controller: string | null;
  joined_at: string | null;
  setup_finished: boolean | null;
  remaining_steps: string[];
  options: JoinOptions | null;
  checks: HealthCheck[];
}
