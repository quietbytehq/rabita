// SPDX-License-Identifier: Apache-2.0
// Unit tests for the wizard rules (run with `npm test`).
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  adminProblem,
  ALL_STEPS,
  buildJoinRequest,
  describeDiscovery,
  domainProblem,
  emptyForm,
  finishView,
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
  assert.deepEqual(ALL_STEPS, ["detect", "packages", "time", "join", "sssd", "access", "home", "display", "verify"]);
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
