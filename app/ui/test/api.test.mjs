// SPDX-License-Identifier: Apache-2.0
// The page's last-resort time limits (run with `npm test`).
import assert from "node:assert/strict";
import { mock, test } from "node:test";

// The page talks to Tauri through window.__TAURI__; this one never answers.
globalThis.window = {
  __TAURI__: {
    core: { invoke: () => new Promise(() => {}) },
    event: { listen: async () => () => undefined },
  },
};
const api = await import("../dist/js/api.js");

const MINUTE = 60_000;

test("every helper command has a last-resort limit longer than the app's own", () => {
  // The Rust side: 30 s first contact, then 45 s (facts), 120 s (slow
  // checks) or 6 minutes (password window), plus subscribing for jobs.
  assert.ok(api.BACKSTOP_MS.detect_system > (30 + 45) * 1000);
  for (const cmd of ["discover_domain", "status", "find_dns_servers"]) {
    assert.ok(api.BACKSTOP_MS[cmd] > (30 + 120) * 1000, cmd);
  }
  for (const cmd of ["join", "leave", "apply_options", "set_dns_server", "restore_dns"]) {
    assert.ok(api.BACKSTOP_MS[cmd] > 30_000 + 3 * 45_000 + 6 * MINUTE, cmd);
  }
  assert.equal(api.BACKSTOP_MS.quit, undefined, "closing the window is never limited");
});

test("a call that never answers ends with a clear error", async () => {
  mock.timers.enable({ apis: ["setTimeout"] });
  try {
    const pending = api.detectSystem();
    const outcome = pending.then(
      () => "answered",
      (e) => e.message,
    );
    mock.timers.tick(api.BACKSTOP_MS.detect_system + 1);
    const message = await outcome;
    assert.match(message, /stopped waiting for its background service after 2 minutes/);
    assert.match(message, /open it again/);
  } finally {
    mock.timers.reset();
  }
});

test("backstop wording", () => {
  assert.match(api.backstopMessage(MINUTE), /after 1 minute\./);
  assert.match(api.backstopMessage(10 * MINUTE), /after 10 minutes\./);
});
