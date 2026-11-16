// SPDX-License-Identifier: Apache-2.0
// Unit tests for the wizard rules (run with `npm test`).
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  adminProblem,
  ALL_STEPS,
  buildJoinRequest,
  buildLeaveRequest,
  checkTone,
  describeDiscovery,
  domainProblem,
  emptyForm,
  finishView,
  formatDate,
  hostnameProblem,
  ipProblem,
  LEAVE_STEP_IDS,
  leaveProblem,
  leaveView,
  looksLikeDomain,
  normaliseAdminUser,
  normaliseDomain,
  optionsProblem,
  parseGroups,
  POST_JOIN_STEPS,
  progressFraction,
  stepLabel,
} from "../dist/js/logic.js";

test("domain names are normalised like the helper does", () => {
  assert.equal(normaliseDomain("  LAB.QuietByte.test. "), "lab.quietbyte.test");
  assert.equal(normaliseDomain("\\\\school.example.org"), "school.example.org");
  assert.ok(looksLikeDomain("school.example.org"));
  assert.ok(!looksLikeDomain("SCHOOL"));
  assert.ok(!looksLikeDomain("bad_name.test"));
  assert.ok(!looksLikeDomain("-x.test"));
});

test("domain problems are explained in plain words", () => {
  assert.equal(domainProblem(""), "Enter your organization's domain name.");
  assert.match(domainProblem("SCHOOL"), /full domain name with dots/);
  assert.match(domainProblem("my school.org"), /doesn't look like a domain/);
  assert.equal(domainProblem("school.example.org"), null);
});

test("admin user names drop a Windows-style domain prefix", () => {
  assert.equal(normaliseAdminUser(" SCHOOL\\joiner "), "joiner");
  assert.equal(normaliseAdminUser("joiner"), "joiner");
  const form = { ...emptyForm(), adminUser: "joiner" };
  assert.equal(adminProblem(form), "Enter the password.");
  assert.equal(adminProblem({ ...form, password: "x" }), null);
  assert.equal(adminProblem({ ...form, adminUser: "jo iner", password: "x" }), "User names can't contain spaces.");
  assert.equal(adminProblem(emptyForm()), "Enter the IT admin user name.");
});

test("groups are split on lines and commas, without blanks or repeats", () => {
  assert.deepEqual(parseGroups("Linux Users\n Staff ,staff\n\n,"), ["Linux Users", "Staff"]);
  const groupsForm = { ...emptyForm(), accessMode: "groups", groupsText: " \n" };
  assert.match(optionsProblem(groupsForm), /at least one group/);
  assert.equal(optionsProblem(emptyForm()), null);
});

test("the join request never contains the password", () => {
  const form = {
    ...emptyForm(),
    domain: "LAB.QuietByte.test",
    adminUser: "LAB\\joiner",
    password: "S3cret!",
    computerOu: " OU=Linux PCs,DC=lab ",
    accessMode: "groups",
    groupsText: "Linux Users\nStaff",
  };
  const req = buildJoinRequest(form);
  assert.deepEqual(req, {
    domain: "lab.quietbyte.test",
    admin_user: "joiner",
    computer_ou: "OU=Linux PCs,DC=lab",
    computer_name: null,
    verify_user: null,
    new_hostname: null,
    options: {
      manual_login: true,
      create_home: true,
      offline_login: true,
      login_access: { mode: "groups", groups: ["Linux Users", "Staff"] },
    },
  });
  assert.ok(!JSON.stringify(req).includes("S3cret"));
});

test("step lists match the helper", () => {
  assert.deepEqual(ALL_STEPS, ["detect", "packages", "time", "name", "join", "sssd", "access", "home", "display", "verify"]);
  assert.deepEqual(POST_JOIN_STEPS, ["sssd", "access", "home", "display", "verify"]);
  assert.equal(stepLabel("home"), "Create home folders on first login");
});

test("progress counts finished steps and half of a running one", () => {
  assert.equal(progressFraction([]), 0);
  assert.equal(progressFraction(["done", "running", "pending", "pending"]), 0.375);
  assert.equal(progressFraction(["done", "skipped", "failed"]), 1);
});

const dc = (host, reachable, offset, extra = {}) => ({
  host, port: 389, priority: 0, weight: 100, reachable, clock_offset_secs: offset,
  in_client_site: false, recommended: false, warning: null, ...extra,
});

test("discovery is summarised with the DC, site and warnings", () => {
  const view = describeDiscovery({
    domain: "lab.quietbyte.test",
    realm: "LAB.QUIETBYTE.TEST",
    found: true,
    controllers: [
      dc("bdc-2021.lab.quietbyte.test", true, 1, { recommended: true, in_client_site: true }),
      dc("dc-b1-4.lab.quietbyte.test", true, 167),
      dc("old-dc1.lab.quietbyte.test", false, null),
    ],
    problem: null,
    client_site: "HQ",
    recommended: "bdc-2021.lab.quietbyte.test",
    majority_clock_offset_secs: 1,
    warnings: ["1 of 3 domain controllers could not be reached (possibly stale DNS records): old-dc1.lab.quietbyte.test."],
  });
  assert.equal(view.tone, "warn");
  assert.equal(view.headline, "Found lab.quietbyte.test.");
  assert.equal(view.lines[0], "2 of 3 domain controllers answered.");
  assert.equal(view.lines[1], "This computer will use bdc-2021.lab.quietbyte.test, in your site “HQ”.");
  assert.match(view.lines[2], /possibly stale DNS records/);
  assert.ok(view.canContinue);
});

test("a large clock difference is mentioned; nothing found blocks Next", () => {
  const found = describeDiscovery({
    domain: "x.test", realm: "X.TEST", found: true,
    controllers: [dc("a.x.test", true, 400, { recommended: true })],
    problem: null, client_site: null, recommended: "a.x.test",
    majority_clock_offset_secs: 400, warnings: [],
  });
  assert.equal(found.tone, "ok");
  assert.match(found.lines.join(" "), /about 7 minutes off/);
  const none = describeDiscovery({
    domain: "x.test", realm: "X.TEST", found: false, controllers: [],
    problem: "No domain controllers were found for 'x.test'.", client_site: null,
    recommended: null, majority_clock_offset_secs: null, warnings: [],
  });
  assert.equal(none.tone, "error");
  assert.ok(!none.canContinue);
  assert.match(none.headline, /No domain controllers/);
});

const result = (extra) => ({ ok: false, summary: "", steps: [], joined_not_finished: false, remaining_steps: [], ...extra });

test("the finish screen offers the right way forward", () => {
  const ok = finishView(result({ ok: true }), "lab.test");
  assert.equal(ok.tone, "ok");
  assert.equal(ok.body, "This computer is now connected to lab.test.");
  const partial = finishView(
    result({
      joined_not_finished: true,
      summary: "This computer is joined to lab.test but not finished. … (rabitactl finish) …",
      steps: [{ id: "home", label: "x", state: "failed", message: "pam-auth-update: profile not found" }],
      remaining_steps: ["home", "verify"],
    }),
    "lab.test",
  );
  assert.equal(partial.tone, "warn");
  assert.ok(partial.canFinishSetup);
  assert.equal(partial.backTo, null);
  assert.match(partial.body, /^This computer has joined lab.test, but setup is not finished/);
  assert.ok(!partial.body.includes("rabitactl"), "no command-line advice in the wizard");
  assert.equal(partial.problem, "pam-auth-update: profile not found");
  assert.deepEqual(partial.remaining, ["Create home folders on first login", "Check that it works"]);
  assert.equal(finishView(result({ summary: "The administrator username or password was not accepted." }), "x").backTo, "admin");
  assert.equal(finishView(result({ summary: "The domain could not be found. Check the domain name." }), "x").backTo, "organization");
  assert.equal(finishView(result({ summary: "Install domain login packages failed: ..." }), "x").backTo, "options");
});

test("computer names follow the directory's rules, like the helper", () => {
  assert.equal(hostnameProblem("lab-pc-01"), null);
  assert.equal(hostnameProblem("  lab-pc-01 "), null);
  assert.match(hostnameProblem("lab-classroom-pc-01"), /19 characters long/);
  assert.match(hostnameProblem("lab_pc"), /letters, numbers and hyphens/);
  assert.match(hostnameProblem("lab.pc"), /letters, numbers and hyphens/);
  assert.match(hostnameProblem("-lab"), /start or end with a hyphen/);
  assert.match(hostnameProblem("lab-"), /start or end with a hyphen/);
  assert.match(hostnameProblem("2024"), /only numbers/);
  assert.match(hostnameProblem(""), /Enter a name/);
  assert.equal(hostnameProblem("a23456789012345"), null, "exactly 15 is allowed");
});

test("the new computer name is sent only when a rename is needed", () => {
  const form = { ...emptyForm(), domain: "lab.quietbyte.test", adminUser: "joiner", newHostname: " lab-pc-12 " };
  assert.equal(buildJoinRequest(form).new_hostname, null);
  assert.equal(buildJoinRequest(form, true).new_hostname, "lab-pc-12");
});

test("DNS server addresses are checked before anything changes", () => {
  assert.equal(ipProblem("10.0.0.10"), null);
  assert.equal(ipProblem(" 192.168.1.1 "), null);
  assert.equal(ipProblem("fd00::10"), null);
  assert.match(ipProblem(""), /Enter the DNS server's IP address/);
  assert.match(ipProblem("10.0.0"), /not an IP address/);
  assert.match(ipProblem("10.0.0.256"), /not an IP address/);
  assert.match(ipProblem("dc1.lab.test"), /not an IP address/);
});

test("not finding the domain offers the DNS fix; finding it does not", () => {
  const notFound = describeDiscovery({
    domain: "lab.quietbyte.test", realm: "LAB.QUIETBYTE.TEST", found: false, controllers: [],
    problem: "No domain controllers were found", client_site: null, recommended: null,
    majority_clock_offset_secs: null, warnings: [],
  });
  assert.ok(notFound.dnsFixable);
  assert.ok(!notFound.canContinue);
  const unreachable = describeDiscovery({
    domain: "lab.quietbyte.test", realm: "LAB.QUIETBYTE.TEST", found: true,
    controllers: [{ host: "dc1", port: 389, priority: 0, weight: 0, reachable: false, clock_offset_secs: null, in_client_site: false, recommended: false, warning: null }],
    problem: "None answered", client_site: null, recommended: null, majority_clock_offset_secs: null, warnings: [],
  });
  assert.ok(!unreachable.dnsFixable, "DNS works when the records were found");
});

test("leaving needs an admin sign-in only when removing the computer account", () => {
  const form = { ...emptyForm(), removeAccount: true, leaveUser: "", leavePassword: "" };
  assert.notEqual(leaveProblem(form), null);
  assert.equal(leaveProblem({ ...form, leaveUser: "LAB\\admin", leavePassword: "pw" }), null);
  assert.equal(leaveProblem({ ...form, removeAccount: false }), null);
  assert.deepEqual(buildLeaveRequest({ ...form, leaveUser: "LAB\\admin", leavePassword: "pw" }), {
    remove_computer_account: true,
    admin_user: "admin",
  });
  assert.deepEqual(buildLeaveRequest({ ...form, removeAccount: false, leaveUser: "admin" }), {
    remove_computer_account: false,
    admin_user: null,
  });
  assert.ok(emptyForm().removeAccount, "removing the account is the default");
});

const leaveResult = (over) => ({
  ok: true, summary: "", steps: [], joined_not_finished: false, remaining_steps: [], left_locally_only: false, ...over,
});

test("leave results explain what happened and what to do next", () => {
  const removed = leaveView(leaveResult({ summary: "This computer has left lab.quietbyte.test, its computer account was removed." }), "lab.quietbyte.test", false);
  assert.equal(removed.tone, "ok");
  assert.equal(removed.title, "This computer has left lab.quietbyte.test");
  const local = leaveView(leaveResult({ left_locally_only: true, summary: "…ask your IT admin to delete it in Active Directory Users and Computers." }), "lab.quietbyte.test", false);
  assert.equal(local.tone, "warn");
  assert.equal(local.title, "Left on this computer only");
  const badPassword = leaveView(leaveResult({ ok: false, summary: "The administrator username or password was not accepted." }), "lab.quietbyte.test", false);
  assert.equal(badPassword.backTo, "leave");
  const notAllowed = leaveView(leaveResult({ ok: false, summary: "That account is not allowed to remove computers from the domain." }), "lab.quietbyte.test", false);
  assert.equal(notAllowed.backTo, "leave");
  const partial = leaveView(leaveResult({ ok: false, summary: "This computer has left lab.quietbyte.test, but some settings could not be put back: x." }), "lab.quietbyte.test", false);
  assert.equal(partial.tone, "warn");
  assert.ok(partial.canRetry);
  assert.equal(partial.backTo, null);
  const undone = leaveView(leaveResult({ summary: "Rabita put back the settings it had changed." }), "", true);
  assert.equal(undone.title, "Changes undone");
});

test("leave steps match the helper's", () => {
  assert.deepEqual(LEAVE_STEP_IDS, ["leave", "restore", "login", "display", "dns", "name", "finish"]);
  assert.equal(stepLabel("dns", true), "Restore the DNS server");
  assert.equal(stepLabel("name"), "Set the computer name");
  assert.equal(stepLabel("name", true), "Restore the computer name");
  assert.ok(ALL_STEPS.includes("name"), "a join can rename the computer");
  assert.equal(ALL_STEPS.indexOf("name"), ALL_STEPS.indexOf("join") - 1, "the rename happens just before joining");
});

test("health checks and dates are shown plainly", () => {
  const c = { id: "x", label: "X", detail: "", warning: false };
  assert.equal(checkTone({ ...c, ok: true }), "ok");
  assert.equal(checkTone({ ...c, ok: true, warning: true }), "warn");
  assert.equal(checkTone({ ...c, ok: false }), "error");
  assert.equal(checkTone({ ...c, ok: null }), "none");
  assert.equal(formatDate("2026-11-13T09:00:00Z"), "13 November 2026");
  assert.equal(formatDate(null), "Not known");
});
