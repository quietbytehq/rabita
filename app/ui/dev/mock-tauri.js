// SPDX-License-Identifier: Apache-2.0
// DEVELOPMENT ONLY — never shipped. Pretends to be the Tauri bridge and the
// helper so the wizard can be clicked through in a normal browser.
// Pick a scenario with window.__RABITA_SCENARIO__ before the page loads:
//   ok (default) · unsupported · long-name · no-dns · wrong-password · not-finished
//   restore-pending · joined · joined-unfinished · joined-unreachable · joined-wrong-password
//   leave-problem
(() => {
  const scenario = window.__RABITA_SCENARIO__ ?? "ok";
  const stepMs = window.__RABITA_STEP_MS__ ?? 220;
  const joined = scenario.startsWith("joined") || scenario === "leave-problem";
  const listeners = new Map();
  const emit = (name, payload) => (listeners.get(name) ?? []).forEach((cb) => cb({ payload }));
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  const JOIN = ["detect", "packages", "time", "name", "join", "sssd", "access", "home", "display", "verify"];
  const LEAVE = ["leave", "restore", "login", "display", "dns", "name", "finish"];
  const LABELS = {
    detect: "Check this computer", packages: "Install domain login software",
    time: "Find the domain and check the clock", name: "Set the computer name", join: "Join the domain",
    sssd: "Set up sign-in", access: "Choose who can sign in", home: "Create home folders on first login",
    display: "Show a username box on the login screen", verify: "Check that it works",
  };
  const LEAVE_LABELS = {
    leave: "Leave the domain", restore: "Restore the original settings", login: "Turn off domain sign-in rules",
    display: "Restore the login screen", dns: "Restore the DNS server", name: "Restore the computer name", finish: "Clean up",
  };
  const longName = scenario === "long-name";
  const DONE = {
    detect: ["done", "Ubuntu 24.04.1 LTS is supported."],
    packages: ["done", "Installed 11 package(s)."],
    time: ["done", "Found bdc-2021.lab.quietbyte.test. Clock is within 1 s of the domain. (2 warning(s), see details)"],
    name: longName ? ["done", "Renamed this computer from “lab-classroom-pc-01” to “lab-classroo-01”."] : ["skipped", "The current name can be used."],
    join: ["done", "Joined lab.quietbyte.test using domain controller bdc-2021.lab.quietbyte.test."],
    sssd: ["done", "Sign-in configured, checked and started."],
    access: ["done", "All domain users"],
    home: ["done", "Home folders will be created on first login."],
    display: ["done", "Login screen updated."],
    verify: ["done", "Kerberos ticket OK; user 'amna' found"],
  };
  const dc = (host, reachable, offset, extra = {}) => ({
    host, port: 389, priority: 0, weight: 100, reachable, clock_offset_secs: offset,
    in_client_site: false, recommended: false, warning: null, ...extra,
  });
  const dnsChange = {
    connection_uuid: "5f1c0a8e-0000-4000-8000-000000000001", connection_name: "Wired connection 1", device: "enp1s0",
    server: "10.0.0.10", domain: "lab.quietbyte.test", original_dns: [], original_dns_search: [],
    original_ignore_auto_dns: false, original_ipv6_ignore_auto_dns: false,
  };
  const state = {
    dnsSet: false,
    joined,
    restorePending: scenario === "restore-pending",
  };
  const unsupported = scenario === "unsupported";
  const system = () => ({
    distro_id: unsupported ? "fedora" : "ubuntu",
    distro_name: unsupported ? "Fedora Linux 41 (Workstation Edition)" : "Ubuntu 24.04.1 LTS",
    version_id: unsupported ? "41" : "24.04",
    family: unsupported ? "fedora" : "debian",
    supported: !unsupported,
    unsupported_reason: unsupported ? "Support for Fedora / RHEL is coming in a later release." : null,
    display_manager: "gdm",
    hostname: longName ? "lab-classroom-pc-01" : "lab-pc-01",
    joined_domain: state.joined ? "lab.quietbyte.test" : null,
    realmd_installed: state.joined,
    sssd_installed: state.joined,
    hostname_problem: longName ? "“lab-classroom-pc-01” is 19 characters long. Computer names in your organization can have at most 15." : null,
    suggested_hostname: longName ? "lab-classroo-01" : null,
    restore_pending: state.restorePending,
    dns_change: state.restorePending || state.dnsSet ? dnsChange : null,
  });
  const options = { manual_login: true, create_home: true, offline_login: true, login_access: { mode: "all" } };
  const check = (id, label, ok, detail, warning = false) => ({ id, label, ok, detail, warning });

  function finish(id, result) {
    emit("rabita:finished", { job: id, ok: result.ok, result_json: JSON.stringify({ joined_not_finished: false, remaining_steps: [], left_locally_only: false, ...result }) });
  }

  async function runJob(id, steps, failAt) {
    const outcomes = [];
    await wait(stepMs);
    for (const step of steps) {
      emit("rabita:progress", { job: id, step, state: "running", message: "" });
      emit("rabita:log", { job: id, line: `${LABELS[step]}…` });
      await wait(stepMs);
      if (step === failAt?.step) {
        emit("rabita:progress", { job: id, step, state: "failed", message: failAt.message });
        emit("rabita:log", { job: id, line: `  (exit status 1) ${failAt.message}` });
        outcomes.push({ id: step, label: LABELS[step], state: "failed", message: failAt.message });
        const rest = steps.slice(steps.indexOf(step));
        finish(id, {
          ok: false, summary: failAt.summary, steps: outcomes,
          joined_not_finished: failAt.joined === true,
          remaining_steps: failAt.joined === true ? rest : [],
        });
        return;
      }
      const [st, message] = DONE[step];
      emit("rabita:progress", { job: id, step, state: st, message });
      outcomes.push({ id: step, label: LABELS[step], state: st, message });
    }
    state.joined = true;
    finish(id, {
      ok: true,
      summary: steps.length === JOIN.length
        ? "This computer is now connected to lab.quietbyte.test."
        : "Setup finished. This computer is fully connected to lab.quietbyte.test.",
      steps: outcomes,
    });
  }

  async function runLeave(id, request, password) {
    window.__RABITA_LAST_LEAVE__ = { request, passwordLength: password.length };
    const undo = !state.joined;
    const outcomes = [];
    const report = (step, st, message) => {
      emit("rabita:progress", { job: id, step, state: st, message });
      outcomes.push({ id: step, label: LEAVE_LABELS[step], state: st, message });
    };
    await wait(stepMs);
    let local = false;
    for (const step of LEAVE) {
      emit("rabita:progress", { job: id, step, state: "running", message: "" });
      await wait(stepMs);
      if (step === "leave") {
        if (undo) { report(step, "skipped", "This computer is not joined."); continue; }
        if (request.remove_computer_account && scenario === "joined-wrong-password") {
          report(step, "failed", "The administrator username or password was not accepted.");
          finish(id, { ok: false, summary: "The administrator username or password was not accepted.", steps: outcomes });
          return;
        }
        if (request.remove_computer_account && scenario === "joined-unreachable") {
          emit("rabita:log", { job: id, line: "Warning: could not reach lab.quietbyte.test to remove the computer account (Couldn't connect to the domain). Leaving on this computer only." });
          local = true;
          report(step, "done", "Left on this computer only. The computer account is still in the directory.");
          continue;
        }
        local = !request.remove_computer_account;
        report(step, "done", local ? "Left on this computer only." : "Left lab.quietbyte.test and removed the computer account.");
        continue;
      }
      if (step === "dns") {
        report(step, state.dnsSet || state.restorePending ? "done" : "skipped",
          state.dnsSet || state.restorePending ? "Put the original DNS setting back on “Wired connection 1”." : "Rabita did not change the DNS server.");
        continue;
      }
      if (step === "login" && scenario === "leave-problem") {
        report(step, "failed", "pam-auth-update failed: profile not found");
        continue;
      }
      if (undo && step !== "finish") { report(step, "skipped", "Nothing to put back."); continue; }
      report(step, "done", step === "name" ? "The computer name was not changed." : "Done.");
    }
    if (scenario === "leave-problem") {
      state.joined = false;
      finish(id, {
        ok: false, steps: outcomes,
        summary: "This computer has left lab.quietbyte.test, but some settings could not be put back: Turn off domain sign-in rules: pam-auth-update failed: profile not found. Open Rabita Setup again to try once more.",
      });
      return;
    }
    const summary = undo
      ? "Rabita put back the settings it had changed."
      : local
        ? "This computer has left lab.quietbyte.test and its original settings are restored. Its computer account (LAB-PC-01) is still in the directory: ask your IT admin to delete it in Active Directory Users and Computers."
        : "This computer has left lab.quietbyte.test, its computer account was removed, and its original settings are restored.";
    state.joined = false;
    state.dnsSet = false;
    state.restorePending = false;
    finish(id, { ok: true, summary, steps: outcomes, left_locally_only: !undo && local });
  }

  const commands = {
    async detect_system() {
      await wait(150);
      return JSON.stringify(system());
    },
    async status() {
      await wait(250);
      const unfinished = scenario === "joined-unfinished";
      const unreachable = scenario === "joined-unreachable";
      return JSON.stringify({
        joined: true, domain: "lab.quietbyte.test", realm: "LAB.QUIETBYTE.TEST",
        computer_name: "LAB-PC-01", domain_controller: "bdc-2021.lab.quietbyte.test",
        joined_at: "2026-11-13T09:00:00Z", setup_finished: !unfinished,
        remaining_steps: unfinished ? ["home", "display", "verify"] : [], options,
        checks: [
          unreachable
            ? check("dc", "Domain controller", false, "No domain controller could be reached")
            : check("dc", "Domain controller", true, "bdc-2021.lab.quietbyte.test is reachable"),
          unreachable
            ? check("dc-health", "Domain controller health", null, "Not checked")
            : check("dc-health", "Domain controller health", true, "4 of 6 reachable. 2 could not be reached (possibly stale DNS records).", true),
          unfinished
            ? check("setup", "Setup", false, "Joined but not finished. Still to do: Create home folders on first login, Show a username box on the login screen, Check that it works. Choose Finish setup in Rabita Setup.")
            : check("setup", "Setup", true, "Finished"),
          check("sssd-service", "Sign-in service", true, "Running"),
          check("login-access", "Who can sign in", true, "All domain users"),
          check("keytab", "Computer account key", true, "Present"),
          unreachable
            ? check("kerberos", "Kerberos sign-in", false, "Cannot contact any KDC for realm 'LAB.QUIETBYTE.TEST'")
            : check("kerberos", "Kerberos sign-in", true, "Ticket obtained as LAB-PC-01$@LAB.QUIETBYTE.TEST"),
          check("time", "Clock", true, "Synchronized"),
        ],
      });
    },
    async discover_domain({ domain }) {
      await wait(400);
      if (domain === "nowhere.test" || (scenario === "no-dns" && !state.dnsSet)) {
        return JSON.stringify({
          domain, realm: domain.toUpperCase(), found: false, controllers: [],
          problem: `No domain controllers were found for '${domain}'. Check the spelling, and make sure this computer uses the organization's DNS server.`,
          client_site: null, recommended: null, majority_clock_offset_secs: null, warnings: [],
        });
      }
      return JSON.stringify({
        domain, realm: domain.toUpperCase(), found: true,
        controllers: [
          dc("bdc-2021.lab.quietbyte.test", true, 1, { recommended: true, in_client_site: true }),
          dc("dc-b.lab.quietbyte.test", true, -2),
          dc("dc-a.lab.quietbyte.test", true, 3),
          dc("dc-b1-4.lab.quietbyte.test", true, 167, { warning: "Its clock is +167 s from this computer's and disagrees with the other domain controllers. It will not be used to set the time." }),
          dc("old-dc1.lab.quietbyte.test", false, null),
          dc("old-dc2.lab.quietbyte.test", false, null),
        ],
        problem: null, client_site: "HQ", recommended: "bdc-2021.lab.quietbyte.test",
        majority_clock_offset_secs: 1,
        warnings: [
          "2 of 6 domain controllers could not be reached (possibly stale DNS records): old-dc1.lab.quietbyte.test, old-dc2.lab.quietbyte.test.",
          "1 domain controller(s) have clocks more than 60 s off: dc-b1-4.lab.quietbyte.test (+167 s). This computer will use bdc-2021.lab.quietbyte.test for time.",
        ],
      });
    },
    async find_dns_servers() {
      await wait(500);
      return JSON.stringify({
        connection_name: "Wired connection 1", device: "enp1s0",
        candidates: ["10.0.0.10", "10.0.0.11"], scanned: 254, problem: null,
      });
    },
    async set_dns_server({ server }) {
      await wait(300);
      window.__RABITA_LAST_DNS__ = server;
      if (server !== "10.0.0.10" && server !== "10.0.0.11") {
        return JSON.stringify({ ok: false, message: `${server} does not know lab.quietbyte.test. Nothing was changed.` });
      }
      state.dnsSet = true;
      return JSON.stringify({
        ok: true,
        message: `This computer now uses ${server} as its DNS server on “Wired connection 1”. Leaving the domain puts the old setting back.`,
      });
    },
    async restore_dns() {
      await wait(150);
      state.dnsSet = false;
      window.__RABITA_DNS_RESTORED__ = true;
      return JSON.stringify({ ok: true, message: "Put the original DNS setting back on “Wired connection 1”." });
    },
    async join({ request, password }) {
      window.__RABITA_LAST_JOIN__ = { request: JSON.parse(request), passwordLength: password.length };
      await wait(200);
      const failAt =
        scenario === "wrong-password"
          ? { step: "join", message: "The administrator username or password was not accepted.", summary: "The administrator username or password was not accepted." }
          : scenario === "not-finished"
            ? {
                step: "home", joined: true,
                message: "Turn on domain sign-in and home folder creation failed: pam-auth-update: profile not found",
                summary: "This computer is joined to lab.quietbyte.test but not finished. Turn on domain sign-in and home folder creation failed: pam-auth-update: profile not found. Until setup is finished, domain sign-in may not work properly (for example, home folders may not be created). Fix the problem, then choose Finish setup in Rabita Setup to complete the remaining steps: Create home folders on first login, Show a username box on the login screen, Check that it works.",
              }
            : null;
      void runJob(1, JOIN, failAt);
      return 1;
    },
    async apply_options() {
      void runJob(2, ["sssd", "access", "home", "display", "verify"], null);
      return 2;
    },
    async leave({ request, password }) {
      await wait(150);
      void runLeave(3, JSON.parse(request), password);
      return 3;
    },
    async quit() {
      window.__RABITA_QUIT__ = true;
    },
  };

  window.__TAURI__ = {
    core: {
      invoke: (cmd, args) => (commands[cmd] ? commands[cmd](args ?? {}) : Promise.reject(`unknown command ${cmd}`)),
    },
    event: {
      listen: async (name, cb) => {
        listeners.set(name, [...(listeners.get(name) ?? []), cb]);
        return () => undefined;
      },
    },
  };
})();
