// SPDX-License-Identifier: Apache-2.0
// DEVELOPMENT ONLY. Clicks through the wizard in Chromium against the mock
// helper (dev/mock-tauri.js), checks the rules on each screen, and saves
// screenshots to docs/screenshots/. Needs Playwright with Chromium:
//   npm run build && NODE_PATH=$(npm root -g) node dev/screenshots.mjs
import { createServer } from "node:http";
import { readFileSync, mkdirSync, existsSync } from "node:fs";
import { extname, join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";
import assert from "node:assert/strict";

const { chromium } = createRequire(import.meta.url)("playwright");
const ui = join(dirname(fileURLToPath(import.meta.url)), "..");
const dist = join(ui, "dist");
const out = join(ui, "..", "..", "docs", "screenshots");
mkdirSync(out, { recursive: true });
const mock = readFileSync(join(ui, "dev", "mock-tauri.js"), "utf8");
const types = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".ttf": "font/ttf" };

const server = createServer((req, res) => {
  const path = join(dist, req.url === "/" ? "index.html" : decodeURIComponent(req.url.split("?")[0]));
  if (!path.startsWith(dist) || !existsSync(path)) {
    res.writeHead(404).end();
    return;
  }
  res.writeHead(200, { "content-type": types[extname(path)] ?? "application/octet-stream" });
  res.end(readFileSync(path));
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const url = `http://127.0.0.1:${server.address().port}/`;
const browser = await chromium.launch();

async function open(scenario, { reducedMotion = "no-preference" } = {}) {
  const page = await browser.newPage({ viewport: { width: 800, height: 560 }, deviceScaleFactor: 2, reducedMotion });
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  page.on("console", (m) => m.type() === "error" && errors.push(m.text()));
  await page.addInitScript(`window.__RABITA_SCENARIO__ = ${JSON.stringify(scenario)}; window.__RABITA_STEP_MS__ = 120;\n${mock}`);
  await page.goto(url);
  await page.evaluate(() => document.fonts.ready);
  page.errors = errors;
  return page;
}

const shot = async (page, name) => {
  await page.waitForTimeout(450); // let the slide-in finish
  await page.screenshot({ path: join(out, `${name}.png`) });
};
const disabled = (page, id) => page.locator(`#${id}`).isDisabled();

async function walkToOptions(page) {
  await page.getByRole("button", { name: "Next ›" }).click();
  assert.ok(await disabled(page, "next"), "Next stays off until the domain is found");
  await page.fill("#domain", "SCHOOL");
  await page.getByRole("button", { name: "Find" }).click();
  await page.getByText("full domain name with dots").waitFor();
  await page.fill("#domain", "LAB.QuietByte.test");
  await page.press("#domain", "Enter");
  await page.getByText("Found lab.quietbyte.test.").waitFor();
  assert.ok(!(await disabled(page, "next")));
  return page;
}

// 1. Happy path, with a screenshot of every screen.
{
  const page = await open("ok");
  await page.getByText("Ubuntu 24.04.1 LTS").waitFor();
  assert.ok(!(await disabled(page, "next")));
  assert.equal(await page.locator("#steps li.active").innerText(), "Welcome");
  await shot(page, "1-welcome");
  await walkToOptions(page);
  await shot(page, "2-your-organization");
  await page.getByRole("button", { name: "Next ›" }).click();
  assert.ok(await disabled(page, "next"), "Next needs a user name and password");
  await page.fill("#admin-user", "LAB\\joiner");
  await page.fill("#password", "S3cret!");
  assert.ok(!(await disabled(page, "next")));
  await shot(page, "3-it-admin-sign-in");
  await page.press("#password", "Enter");
  await page.getByText("Who can sign in").waitFor();
  await page.click("#access-groups");
  assert.ok(await disabled(page, "next"), "Groups chosen but none typed");
  await page.fill("#groups", "Linux Users\nStaff");
  assert.ok(!(await disabled(page, "next")));
  await shot(page, "4-choose-options");
  await page.getByRole("button", { name: "Connect" }).click();
  await page.locator('li[data-step="join"].done').waitFor();
  await page.screenshot({ path: join(out, "5-setting-up.png") });
  const sent = await page.evaluate(() => window.__RABITA_LAST_JOIN__);
  assert.equal(sent.request.domain, "lab.quietbyte.test");
  assert.equal(sent.request.admin_user, "joiner");
  assert.deepEqual(sent.request.options.login_access, { mode: "groups", groups: ["Linux Users", "Staff"] });
  assert.ok(!JSON.stringify(sent.request).includes("S3cret"), "password is not part of the request");
  await page.getByText("All done").waitFor();
  await page.waitForTimeout(1100); // the tick draws itself
  await page.screenshot({ path: join(out, "6-finish.png") });
  await page.getByRole("button", { name: "Close" }).click();
  assert.ok(await page.evaluate(() => window.__RABITA_QUIT__ === true));
  assert.deepEqual(page.errors, []);
  await page.close();
}

// 2. Joined but not finished, then Finish setup.
{
  const page = await open("not-finished");
  await page.getByText("Ubuntu 24.04.1 LTS").waitFor();
  await walkToOptions(page);
  await page.getByRole("button", { name: "Next ›" }).click();
  await page.fill("#admin-user", "joiner");
  await page.fill("#password", "pw");
  await page.getByRole("button", { name: "Next ›" }).click();
  await page.getByRole("button", { name: "Connect" }).click();
  await page.getByText("Almost done").waitFor();
  await page.getByRole("button", { name: "Show details" }).click();
  await shot(page, "7-joined-but-not-finished");
  assert.ok(await page.locator("#back").isHidden(), "no going back once the computer is joined");
  await page.getByRole("button", { name: "Finish setup" }).click();
  await page.getByText("Finishing setup…").waitFor();
  assert.equal(await page.locator(".checklist li").count(), 5, "only the post-join steps run");
  await page.getByText("All done").waitFor();
  assert.deepEqual(page.errors, []);
  await page.close();
}

// 3. Wrong password goes back to the sign-in screen.
{
  const page = await open("wrong-password");
  await page.getByText("Ubuntu 24.04.1 LTS").waitFor();
  await walkToOptions(page);
  await page.getByRole("button", { name: "Next ›" }).click();
  await page.fill("#admin-user", "joiner");
  await page.fill("#password", "bad");
  await page.getByRole("button", { name: "Next ›" }).click();
  await page.getByRole("button", { name: "Connect" }).click();
  await page.getByText("This computer was not connected").waitFor();
  await page.getByRole("button", { name: "‹ Back" }).click();
  await page.locator("#admin-user").waitFor();
  assert.equal(await page.inputValue("#password"), "", "the password is not kept after a join starts");
  assert.deepEqual(page.errors, []);
  await page.close();
}

// 4. Unsupported system and an unfinished earlier join, seen on Welcome.
{
  const page = await open("unsupported");
  await page.getByText("Not supported yet").waitFor();
  assert.ok(await disabled(page, "next"));
  await shot(page, "8-not-supported");
  await page.close();
  const joined = await open("joined-unfinished");
  await joined.getByText("but setup is not finished").waitFor();
  assert.ok(await joined.getByRole("button", { name: "Finish setup" }).isVisible());
  await joined.close();
}

// 5. Cancel asks first; reduced motion stops every animation.
{
  const page = await open("ok", { reducedMotion: "reduce" });
  await page.getByText("Ubuntu 24.04.1 LTS").waitFor();
  await page.getByRole("button", { name: "Next ›" }).click();
  await page.keyboard.press("Escape");
  await page.getByText("Close Rabita Setup?").waitFor();
  await page.getByRole("button", { name: "Keep going" }).click();
  const animated = await page.evaluate(() =>
    [...document.querySelectorAll("*")].filter((el) => getComputedStyle(el).animationName !== "none").length,
  );
  assert.equal(animated, 0, "reduced motion: nothing animates");
  await page.close();
}

// 6. A computer name that is too long is fixed on Welcome and sent with the join.
{
  const page = await open("long-name");
  await page.getByText("This computer needs a new name").waitFor();
  assert.equal(await page.inputValue("#new-name"), "lab-classroo-01", "the suggestion is filled in");
  await page.fill("#new-name", "lab_pc!");
  await page.getByText("Use only letters, numbers and hyphens.").waitFor();
  assert.ok(await disabled(page, "next"), "an invalid name blocks Next");
  await page.fill("#new-name", "lab-pc-room12");
  assert.ok(!(await disabled(page, "next")));
  await shot(page, "9-rename-computer");
  await walkToOptions(page);
  await page.getByRole("button", { name: "Next ›" }).click();
  await page.fill("#admin-user", "joiner");
  await page.fill("#password", "pw");
  await page.getByRole("button", { name: "Next ›" }).click();
  await page.getByRole("button", { name: "Connect" }).click();
  await page.getByText("All done").waitFor();
  const sent = await page.evaluate(() => window.__RABITA_LAST_JOIN__);
  assert.equal(sent.request.new_hostname, "lab-pc-room12");
  assert.equal(await page.locator(".checklist li").count(), 0, "finish screen replaced the checklist");
  assert.deepEqual(page.errors, []);
  await page.close();
}

// 7. The domain can't be found: detect a DNS server, use it, and find the domain.
{
  const page = await open("no-dns");
  await page.getByText("Ubuntu 24.04.1 LTS").waitFor();
  await page.getByRole("button", { name: "Next ›" }).click();
  await page.fill("#domain", "lab.quietbyte.test");
  await page.press("#domain", "Enter");
  await page.getByText("lab.quietbyte.test was not found").waitFor();
  assert.ok(await disabled(page, "next"));
  await page.fill("#dns-server", "10.0.0");
  await page.getByRole("button", { name: "Use this server" }).click();
  await page.getByText("is not an IP address").waitFor();
  await page.getByRole("button", { name: "Look for it on this network" }).click();
  await page.locator(".candidates li", { hasText: "10.0.0.10" }).waitFor();
  await shot(page, "10-dns-server");
  const dnsOverflow = await page.evaluate(() => {
    const c = document.getElementById("content");
    return c.scrollHeight - c.clientHeight;
  });
  assert.ok(dnsOverflow <= 0, `DNS screen overflows by ${dnsOverflow}px`);
  await page.locator(".candidates li", { hasText: "10.0.0.10" }).getByRole("button", { name: "Use" }).click();
  await page.getByText("Found lab.quietbyte.test.").waitFor();
  await page.getByText("now uses 10.0.0.10 as its DNS server").waitFor();
  assert.equal(await page.evaluate(() => window.__RABITA_LAST_DNS__), "10.0.0.10");
  assert.ok(!(await disabled(page, "next")));
  // Cancelling now offers to put the DNS setting back.
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "Undo and close" }).click();
  await page.waitForFunction(() => window.__RABITA_QUIT__ === true);
  assert.ok(await page.evaluate(() => window.__RABITA_DNS_RESTORED__ === true));
  assert.deepEqual(page.errors, []);
  await page.close();
}

async function openStatus(scenario) {
  const page = await open(scenario);
  await page.getByText("Connected to lab.quietbyte.test").first().waitFor();
  await page.getByText("Computer account key").waitFor();
  return page;
}

async function leave(page, { remove = true, user = "admin", password = "pw" } = {}) {
  await page.getByRole("button", { name: "Leave domain…" }).click();
  await page.getByText("Leave lab.quietbyte.test", { exact: true }).waitFor();
  if (remove) {
    assert.ok(await disabled(page, "next"), "removing the account needs an admin sign-in");
    await page.fill("#leave-user", user);
    await page.fill("#leave-password", password);
  } else {
    await page.getByText("Also remove this computer from").click();
    assert.ok(!(await page.isChecked("#remove-account")));
    await page.getByText("The computer account stays in the directory").waitFor();
  }
  assert.ok(!(await disabled(page, "next")));
}

// 8. Joined: Status, then Leave domain with removal from the directory.
{
  const page = await openStatus("joined");
  assert.equal(await page.locator("#steps li.active").innerText(), "Status");
  assert.equal(await page.locator(".checks li").count(), 8);
  await shot(page, "11-status");
  await leave(page);
  await shot(page, "12-leave-domain");
  await page.getByRole("button", { name: "Leave domain", exact: true }).click();
  await page.getByText("Leave lab.quietbyte.test?").waitFor();
  await page.getByRole("button", { name: "Keep connected" }).click();
  assert.ok(!(await page.locator("#dialog").isVisible()), "nothing happens without confirming");
  await page.getByRole("button", { name: "Leave domain", exact: true }).click();
  await page.getByRole("button", { name: "Leave", exact: true }).click();
  await page.getByText("This computer has left lab.quietbyte.test").first().waitFor();
  await page.waitForTimeout(1100);
  await page.screenshot({ path: join(out, "13-left-domain.png") });
  const sent = await page.evaluate(() => window.__RABITA_LAST_LEAVE__);
  assert.deepEqual(sent.request, { remove_computer_account: true, admin_user: "admin" });
  assert.equal(sent.passwordLength, 2);
  assert.equal(await page.locator("#steps li.active").innerText(), "Done");
  assert.deepEqual(page.errors, []);
  await page.close();
}

// 9. The domain can't be reached: Rabita leaves locally and says what to delete.
{
  const page = await openStatus("joined-unreachable");
  assert.ok((await page.locator(".checks li.check-error").count()) >= 1, "failed checks are marked");
  await leave(page);
  await page.getByRole("button", { name: "Leave domain", exact: true }).click();
  await page.getByRole("button", { name: "Leave", exact: true }).click();
  await page.getByText("Left on this computer only").waitFor();
  await page.getByText("Active Directory Users and Computers").waitFor();
  await page.waitForTimeout(1100);
  await page.screenshot({ path: join(out, "14-left-locally-only.png") });
  assert.deepEqual(page.errors, []);
  await page.close();
}

// 10. A wrong admin password on Leave goes back with the password cleared.
{
  const page = await openStatus("joined-wrong-password");
  await leave(page, { password: "bad" });
  await page.getByRole("button", { name: "Leave domain", exact: true }).click();
  await page.getByRole("button", { name: "Leave", exact: true }).click();
  await page.getByText("This computer has not left lab.quietbyte.test").waitFor();
  await page.getByRole("button", { name: "‹ Back" }).click();
  await page.locator("#leave-user").waitFor();
  assert.equal(await page.inputValue("#leave-user"), "admin");
  assert.equal(await page.inputValue("#leave-password"), "");
  assert.deepEqual(page.errors, []);
  await page.close();
}

// 11. Leave on this computer only (no admin account needed).
{
  const page = await openStatus("joined");
  await leave(page, { remove: false });
  await page.getByRole("button", { name: "Leave domain", exact: true }).click();
  await page.getByRole("button", { name: "Leave", exact: true }).click();
  await page.getByText("Left on this computer only").waitFor();
  const sent = await page.evaluate(() => window.__RABITA_LAST_LEAVE__);
  assert.deepEqual(sent.request, { remove_computer_account: false, admin_user: null });
  assert.equal(sent.passwordLength, 0);
  await page.close();
}

// 12. Settings left by an unfinished attempt can be undone from Welcome.
{
  const page = await open("restore-pending");
  await page.getByText("Settings from an earlier attempt").waitFor();
  await page.getByRole("button", { name: "Undo changes" }).click();
  await page.getByText("Changes undone").waitFor();
  await page.getByText("Rabita put back the settings it had changed.").waitFor();
  assert.deepEqual(page.errors, []);
  await page.close();
}

// 13. Some settings could not be put back: offer Try again.
{
  const page = await openStatus("leave-problem");
  await leave(page, { remove: false });
  await page.getByRole("button", { name: "Leave domain", exact: true }).click();
  await page.getByRole("button", { name: "Leave", exact: true }).click();
  await page.getByText("Not everything was put back").waitFor();
  assert.ok(await page.getByRole("button", { name: "Try again" }).isVisible());
  await page.close();
}

// 15. The background service does not answer: a clear reason, and Try again.
{
  const page = await open("helper-silent");
  await page.getByText("Rabita Setup can't check this computer.").waitFor();
  await page.getByText("did not answer within 30 seconds").waitFor();
  await page.getByText("leftover test installation").waitFor();
  assert.ok(await disabled(page, "next"), "nothing can go ahead without the service");
  assert.equal(await page.locator(".working").count(), 0, "no spinner left running");
  await shot(page, "15-service-not-answering");
  await page.getByRole("button", { name: "Try again" }).click();
  await page.getByText("Ubuntu 24.04.1 LTS").waitFor();
  assert.ok(!(await disabled(page, "next")));
  assert.ok(await page.locator("#extra").isHidden(), "Try again goes away once it works");
  assert.deepEqual(page.errors, []);
  await page.close();
}

// 16. The service goes away during a job: the checklist stops, and says what to do.
{
  const page = await open("job-lost");
  await page.getByText("Ubuntu 24.04.1 LTS").waitFor();
  await walkToOptions(page);
  await page.getByRole("button", { name: "Next ›" }).click();
  await page.fill("#admin-user", "joiner");
  await page.fill("#password", "pw");
  await page.getByRole("button", { name: "Next ›" }).click();
  await page.getByRole("button", { name: "Connect" }).click();
  await page.getByText("Rabita Setup lost track of this job").waitFor();
  await page.getByText("open it again").waitFor();
  assert.ok(await page.locator("#back").isHidden(), "no going back to a half-done join");
  assert.ok(await page.locator("#extra").isHidden(), "no Finish setup on a guess");
  assert.ok(await page.getByRole("button", { name: "Close" }).isVisible());
  assert.deepEqual(page.errors, []);
  await page.close();
}

// 14. Every screen fits the 800×560 window without scrolling the page.
{
  for (const scenario of ["joined", "long-name", "joined-unfinished"]) {
    const page = await open(scenario);
    await page.waitForTimeout(900);
    const overflow = await page.evaluate(() => {
      const c = document.getElementById("content");
      return c.scrollHeight - c.clientHeight;
    });
    assert.ok(overflow <= 0, `${scenario}: content overflows by ${overflow}px`);
    await page.close();
  }
}

await browser.close();
server.close();
console.log(`UI walk-through passed; screenshots in ${out}`);
