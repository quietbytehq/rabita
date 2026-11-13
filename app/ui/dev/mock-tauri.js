// SPDX-License-Identifier: Apache-2.0
// DEVELOPMENT ONLY — never shipped. Pretends to be the Tauri bridge and the
// helper so the wizard can be clicked through in a normal browser.
// Pick a scenario with window.__RABITA_SCENARIO__ before the page loads:
//   ok (default) · unsupported · joined-unfinished · wrong-password · not-finished
(() => {
  const scenario = window.__RABITA_SCENARIO__ ?? "ok";
  const stepMs = window.__RABITA_STEP_MS__ ?? 220;
  const listeners = new Map();
  const emit = (name, payload) => (listeners.get(name) ?? []).forEach((cb) => cb({ payload }));
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  const JOIN = ["detect", "packages", "time", "join", "sssd", "access", "home", "display", "verify"];
  const LABELS = {
    detect: "Check this computer", packages: "Install domain login software",
    time: "Find the domain and check the clock", join: "Join the domain", sssd: "Set up sign-in",
    access: "Choose who can sign in", home: "Create home folders on first login",
    display: "Show a username box on the login screen", verify: "Check that it works",
  };
  const DONE = {
    detect: ["done", "Ubuntu 24.04.1 LTS is supported."],
    packages: ["done", "Installed 11 package(s)."],
    time: ["done", "Found bdc-2021.lab.quietbyte.test. Clock is within 1 s of the domain. (2 warning(s), see details)"],
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
  const system = {
    distro_id: scenario === "unsupported" ? "fedora" : "ubuntu",
    distro_name: scenario === "unsupported" ? "Fedora Linux 41 (Workstation Edition)" : "Ubuntu 24.04.1 LTS",
    version_id: scenario === "unsupported" ? "41" : "24.04",
    family: scenario === "unsupported" ? "fedora" : "debian",
    supported: scenario !== "unsupported",
    unsupported_reason: scenario === "unsupported" ? "Support for Fedora / RHEL is coming in a later release." : null,
    display_manager: "gdm",
    hostname: "lab-pc-01",
    joined_domain: scenario === "joined-unfinished" ? "lab.quietbyte.test" : null,
    realmd_installed: false,
    sssd_installed: false,
  };
  const options = { manual_login: true, create_home: true, offline_login: true, login_access: { mode: "all" } };

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
        emit("rabita:finished", {
          job: id, ok: false,
          result_json: JSON.stringify({
            ok: false, summary: failAt.summary, steps: outcomes,
            joined_not_finished: failAt.joined === true,
            remaining_steps: failAt.joined === true ? rest : [],
          }),
        });
        return;
      }
      const [state, message] = DONE[step];
      emit("rabita:progress", { job: id, step, state, message });
      outcomes.push({ id: step, label: LABELS[step], state, message });
    }
    emit("rabita:finished", {
      job: id, ok: true,
      result_json: JSON.stringify({
        ok: true,
        summary: steps.length === JOIN.length
          ? "This computer is now connected to lab.quietbyte.test."
          : "Setup finished. This computer is fully connected to lab.quietbyte.test.",
        steps: outcomes, joined_not_finished: false, remaining_steps: [],
      }),
    });
  }

  const commands = {
    async detect_system() {
      await wait(150);
      return JSON.stringify(system);
    },
    async status() {
      return JSON.stringify({
        joined: true, domain: "lab.quietbyte.test", realm: "LAB.QUIETBYTE.TEST",
        computer_name: "LAB-PC-01", domain_controller: "bdc-2021.lab.quietbyte.test",
        joined_at: "2026-11-13T09:00:00Z", setup_finished: false,
        remaining_steps: ["home", "display", "verify"], options, checks: [],
      });
    },
    async discover_domain({ domain }) {
      await wait(400);
      if (domain === "nowhere.test") {
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
                summary: "This computer is joined to lab.quietbyte.test but not finished. Turn on domain sign-in and home folder creation failed: pam-auth-update: profile not found. Until setup is finished, domain sign-in may not work properly (for example, home folders may not be created). Fix the problem, then run Finish setup (rabitactl finish) to complete the remaining steps: Create home folders on first login, Show a username box on the login screen, Check that it works.",
              }
            : null;
      void runJob(1, JOIN, failAt);
      return 1;
    },
    async apply_options() {
      void runJob(2, ["sssd", "access", "home", "display", "verify"], null);
      return 2;
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
