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

await browser.close();
server.close();
console.log(`UI walk-through passed; screenshots in ${out}`);
