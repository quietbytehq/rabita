// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

// Wizard rules with no DOM access, so they can be unit-tested with
// `node --test`. Plain language lives here too, in one place.

import type {
  DiscoveryResult,
  DisplayManager,
  HealthCheck,
  JobResult,
  JoinOptions,
  JoinRequest,
  LeaveRequest,
  StepState,
} from "./types.js";

/** Join steps in order, with the same ids and labels the helper uses. */
export const JOIN_STEPS: ReadonlyArray<{ id: string; label: string }> = [
  { id: "detect", label: "Check this computer" },
  { id: "packages", label: "Install domain login software" },
  { id: "time", label: "Find the domain and check the clock" },
  { id: "name", label: "Set the computer name" },
  { id: "join", label: "Join the domain" },
  { id: "sssd", label: "Set up sign-in" },
  { id: "access", label: "Choose who can sign in" },
  { id: "home", label: "Create home folders on first login" },
  { id: "display", label: "Show a username box on the login screen" },
  { id: "verify", label: "Check that it works" },
];

/** Steps that run after the computer account exists ("Finish setup"). */
export const POST_JOIN_STEPS: readonly string[] = ["sssd", "access", "home", "display", "verify"];

/** Every step id, for a full join. */
export const ALL_STEPS: readonly string[] = JOIN_STEPS.map((s) => s.id);

/** Leave steps in order, with the same ids and labels the helper uses. */
export const LEAVE_STEPS: ReadonlyArray<{ id: string; label: string }> = [
  { id: "leave", label: "Leave the domain" },
  { id: "restore", label: "Restore the original settings" },
  { id: "login", label: "Turn off domain sign-in rules" },
  { id: "display", label: "Restore the login screen" },
  { id: "dns", label: "Restore the DNS server" },
  { id: "name", label: "Restore the computer name" },
  { id: "finish", label: "Clean up" },
];

export const LEAVE_STEP_IDS: readonly string[] = LEAVE_STEPS.map((s) => s.id);

/** Label of a join step (or a leave step when `leave` is true). */
export function stepLabel(id: string, leave = false): string {
  const list = leave ? LEAVE_STEPS : JOIN_STEPS;
  return list.find((s) => s.id === id)?.label ?? id;
}

/** What the person typed, mirrored from the form controls. */
export interface FormState {
  domain: string;
  adminUser: string;
  password: string;
  computerName: string;
  computerOu: string;
  verifyUser: string;
  manualLogin: boolean;
  createHome: boolean;
  offlineLogin: boolean;
  accessMode: "all" | "groups";
  groupsText: string;
  /** New computer name, when the current one is invalid for the directory. */
  newHostname: string;
  /** DNS server typed on "Your organization". */
  dnsServer: string;
  /** Leave: also remove the computer account from the directory. */
  removeAccount: boolean;
  leaveUser: string;
  leavePassword: string;
}

export function emptyForm(): FormState {
  return {
    domain: "",
    adminUser: "",
    password: "",
    computerName: "",
    computerOu: "",
    verifyUser: "",
    manualLogin: true,
    createHome: true,
    offlineLogin: true,
    accessMode: "all",
    groupsText: "",
    newHostname: "",
    dnsServer: "",
    removeAccount: true,
    leaveUser: "",
    leavePassword: "",
  };
}

/** Same rules as the helper: why a computer name can't be used, or null. */
export function hostnameProblem(name: string): string | null {
  const n = name.trim();
  if (n === "") return "Enter a name for this computer.";
  if (n.length > 15) return `“${n}” is ${n.length} characters long. Use at most 15.`;
  if (!/^[A-Za-z0-9-]+$/.test(n)) return "Use only letters, numbers and hyphens.";
  if (n.startsWith("-") || n.endsWith("-")) return "The name can't start or end with a hyphen.";
  if (/^[0-9]+$/.test(n)) return "The name can't be only numbers.";
  return null;
}

/** Why `text` is not a usable DNS server address, or null. */
export function ipProblem(text: string): string | null {
  const t = text.trim();
  if (t === "") return "Enter the DNS server's IP address, for example 10.0.0.10.";
  const parts = t.split(".");
  const ipv4 =
    parts.length === 4 && parts.every((p) => /^[0-9]{1,3}$/.test(p) && Number(p) <= 255);
  const ipv6 = t.includes(":") && /^[0-9A-Fa-f:]+$/.test(t);
  return ipv4 || ipv6 ? null : `“${t}” is not an IP address. It should look like 10.0.0.10.`;
}

/** Leave form: problem with the admin sign-in, or null when ready. */
export function leaveProblem(form: FormState): string | null {
  if (!form.removeAccount) return null;
  return adminProblem({ ...form, adminUser: form.leaveUser, password: form.leavePassword });
}

export function buildLeaveRequest(form: FormState): LeaveRequest {
  return {
    remove_computer_account: form.removeAccount,
    admin_user: form.removeAccount ? normaliseAdminUser(form.leaveUser) : null,
  };
}

/** How a health check is shown. */
export function checkTone(c: HealthCheck): "ok" | "warn" | "error" | "none" {
  if (c.ok === null) return "none";
  if (!c.ok) return "error";
  return c.warning ? "warn" : "ok";
}

const MONTHS = [
  "January", "February", "March", "April", "May", "June",
  "July", "August", "September", "October", "November", "December",
];

/** "2026-11-13T09:00:00Z" -> "13 November 2026" (UTC, no locale surprises). */
export function formatDate(iso: string | null): string {
  if (iso === null) return "Not known";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return `${d.getUTCDate()} ${MONTHS[d.getUTCMonth()] ?? ""} ${d.getUTCFullYear()}`;
}

/** Same rules as the helper's `normalise_domain`. */
export function normaliseDomain(input: string): string {
  return input.trim().replace(/\.+$/, "").replace(/^\\\\/, "").toLowerCase();
}

/** Same rules as the helper's `looks_like_domain`. */
export function looksLikeDomain(input: string): boolean {
  const d = normaliseDomain(input);
  if (d === "" || !d.includes(".")) return false;
  return d
    .split(".")
    .every((l) => l !== "" && /^[a-z0-9-]+$/.test(l) && !l.startsWith("-") && !l.endsWith("-"));
}

/** A plain-language problem with the domain name, or null if it looks right. */
export function domainProblem(input: string): string | null {
  const d = normaliseDomain(input);
  if (d === "") return "Enter your organization's domain name.";
  if (!d.includes(".")) {
    return "Enter the full domain name with dots, for example school.example.org, not the short name.";
  }
  if (!looksLikeDomain(d)) {
    return "That doesn't look like a domain name. Use letters, numbers, dots and hyphens, for example school.example.org.";
  }
  return null;
}

/**
 * The admin user name as realm expects it. People used to Windows often
 * type `SCHOOL\joiner`; realm wants just `joiner`.
 */
export function normaliseAdminUser(input: string): string {
  const t = input.trim();
  const slash = t.lastIndexOf("\\");
  return slash >= 0 ? t.slice(slash + 1) : t;
}

export function adminProblem(form: FormState): string | null {
  const user = normaliseAdminUser(form.adminUser);
  if (user === "") return "Enter the IT admin user name.";
  if (/\s/.test(user)) return "User names can't contain spaces.";
  if (form.password === "") return "Enter the password.";
  return null;
}

/** Group names, one per line or separated by commas; blanks and repeats removed. */
export function parseGroups(text: string): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const raw of text.split(/[\n,]/)) {
    const g = raw.trim();
    if (g === "" || seen.has(g.toLowerCase())) continue;
    seen.add(g.toLowerCase());
    out.push(g);
  }
  return out;
}

export function optionsProblem(form: FormState): string | null {
  if (form.accessMode === "groups" && parseGroups(form.groupsText).length === 0) {
    return "Enter at least one group, or choose everyone.";
  }
  return null;
}

export function buildOptions(form: FormState): JoinOptions {
  return {
    manual_login: form.manualLogin,
    create_home: form.createHome,
    offline_login: form.offlineLogin,
    login_access:
      form.accessMode === "groups"
        ? { mode: "groups", groups: parseGroups(form.groupsText) }
        : { mode: "all" },
  };
}

function optional(s: string): string | null {
  const t = s.trim();
  return t === "" ? null : t;
}

/**
 * The `Join` request body. The password is deliberately not part of it.
 * `rename` is true when the computer's current name can't be used.
 */
export function buildJoinRequest(form: FormState, rename = false): JoinRequest {
  return {
    domain: normaliseDomain(form.domain),
    admin_user: normaliseAdminUser(form.adminUser),
    computer_ou: optional(form.computerOu),
    computer_name: optional(form.computerName),
    verify_user: optional(form.verifyUser),
    new_hostname: rename ? optional(form.newHostname) : null,
    options: buildOptions(form),
  };
}

export function displayManagerName(dm: DisplayManager): string {
  switch (dm) {
    case "gdm":
      return "GNOME (GDM)";
    case "sddm":
      return "KDE (SDDM)";
    case "light-dm":
      return "LightDM";
    case "unknown":
      return "Not detected";
  }
}

/** How far along a job is, from 0 to 1, for the progress bar. */
export function progressFraction(states: ReadonlyArray<StepState>): number {
  if (states.length === 0) return 0;
  let score = 0;
  for (const s of states) {
    if (s === "done" || s === "skipped" || s === "failed") score += 1;
    else if (s === "running") score += 0.5;
  }
  return Math.min(1, score / states.length);
}

export type Tone = "ok" | "warn" | "error";

export interface DiscoveryView {
  tone: Tone;
  headline: string;
  lines: string[];
  canContinue: boolean;
  /** Nothing found in DNS: offer to set the organization's DNS server. */
  dnsFixable: boolean;
}

/** Turns a discovery result into a few plain sentences. */
export function describeDiscovery(r: DiscoveryResult): DiscoveryView {
  const rec = r.controllers.find((c) => c.recommended);
  if (!r.found || r.recommended === null || rec === undefined) {
    return {
      tone: "error",
      headline: r.problem ?? `No domain controllers for ${r.domain} answered.`,
      lines: [],
      canContinue: false,
      dnsFixable: !r.found,
    };
  }
  const reachable = r.controllers.filter((c) => c.reachable).length;
  const lines: string[] = [];
  lines.push(
    reachable === r.controllers.length
      ? `${plural(reachable, "domain controller")} answered.`
      : `${reachable} of ${plural(r.controllers.length, "domain controller")} answered.`,
  );
  lines.push(
    `This computer will use ${rec.host}${r.client_site !== null && rec.in_client_site ? `, in your site “${r.client_site}”` : ""}.`,
  );
  const offset = r.majority_clock_offset_secs;
  if (offset !== null && Math.abs(offset) > 120) {
    lines.push(
      `This computer's clock is about ${describeSeconds(Math.abs(offset))} off. Rabita will correct it during setup.`,
    );
  }
  lines.push(...r.warnings);
  return {
    tone: r.warnings.length > 0 ? "warn" : "ok",
    headline: `Found ${r.domain}.`,
    lines,
    canContinue: true,
    dnsFixable: false,
  };
}

function plural(n: number, word: string): string {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

function describeSeconds(s: number): string {
  if (s < 90) return `${Math.round(s)} seconds`;
  return `${Math.round(s / 60)} minutes`;
}

export type ScreenId =
  | "welcome"
  | "organization"
  | "admin"
  | "options"
  | "setup"
  | "finish"
  // Managing a joined computer:
  | "status"
  | "leave"
  | "leaving"
  | "left";

export interface FinishView {
  tone: Tone;
  title: string;
  body: string;
  /** What went wrong, in the helper's words, when something did. */
  problem: string | null;
  /** Labels of steps still to run (joined but not finished). */
  remaining: string[];
  /** The computer is joined but some steps did not run: offer Finish setup. */
  canFinishSetup: boolean;
  /** Where ‹ Back should take the person to fix the problem, if anywhere. */
  backTo: ScreenId | null;
}

/** What the Finish screen says about a job result. */
export function finishView(result: JobResult, domain: string): FinishView {
  if (result.ok) {
    return {
      tone: "ok",
      title: "All done",
      body: `This computer is now connected to ${domain}.`,
      problem: null,
      remaining: [],
      canFinishSetup: false,
      backTo: null,
    };
  }
  if (result.joined_not_finished) {
    // The helper's summary is written for the command line; say it our way.
    const failed = result.steps.find((s) => s.state === "failed");
    return {
      tone: "warn",
      title: "Almost done",
      body: `This computer has joined ${domain}, but setup is not finished. Until it is, signing in with a work account may not work properly.`,
      problem: failed?.message ?? result.summary,
      remaining: result.remaining_steps.map((id) => stepLabel(id)),
      canFinishSetup: true,
      backTo: null,
    };
  }
  const s = result.summary.toLowerCase();
  let backTo: ScreenId = "options";
  if (s.includes("password") || s.includes("username") || s.includes("allowed to join") || s.includes("organizational unit")) {
    backTo = "admin";
  } else if (s.includes("domain could not be found") || s.includes("no domain controller") || s.includes("full domain name")) {
    backTo = "organization";
  }
  return {
    tone: "error",
    title: "This computer was not connected",
    body: result.summary,
    problem: null,
    remaining: [],
    canFinishSetup: false,
    backTo,
  };
}

export interface LeaveView {
  tone: Tone;
  title: string;
  body: string;
  /** Where ‹ Back should go to fix the problem (wrong admin password). */
  backTo: ScreenId | null;
  /** Some settings were not put back: offer to try again. */
  canRetry: boolean;
}

/** What the screen after Leave (or Undo) says. */
export function leaveView(result: JobResult, domain: string, undo: boolean): LeaveView {
  if (result.ok && undo) {
    return { tone: "ok", title: "Changes undone", body: result.summary, backTo: null, canRetry: false };
  }
  if (result.ok && result.left_locally_only) {
    return { tone: "warn", title: "Left on this computer only", body: result.summary, backTo: null, canRetry: false };
  }
  if (result.ok) {
    return { tone: "ok", title: `This computer has left ${domain}`, body: result.summary, backTo: null, canRetry: false };
  }
  const s = result.summary.toLowerCase();
  if (s.includes("password") || s.includes("not allowed to remove") || s.includes("it admin account")) {
    return { tone: "error", title: `This computer has not left ${domain}`, body: result.summary, backTo: "leave", canRetry: false };
  }
  if (s.includes("could not be put back")) {
    return { tone: "warn", title: "Not everything was put back", body: result.summary, backTo: null, canRetry: true };
  }
  return {
    tone: "error",
    title: undo ? "The changes were not undone" : `This computer has not left ${domain}`,
    body: result.summary,
    backTo: undo ? null : "leave",
    canRetry: undo,
  };
}
