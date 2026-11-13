// SPDX-License-Identifier: Apache-2.0
// Renders the logo SVG to the PNG sizes the desktop and the app window need.
// Developer tool: needs Playwright with Chromium. Run from the repo root:
//   NODE_PATH=$(npm root -g) node packaging/icons/render-icons.mjs
import { readFileSync, copyFileSync } from "node:fs";
import { createRequire } from "node:module";

// Playwright is usually installed globally; `require` honours NODE_PATH.
const { chromium } = createRequire(import.meta.url)("playwright");

const svg = readFileSync("packaging/icons/org.quietbyte.Rabita.svg", "utf8");
const browser = await chromium.launch();
for (const size of [32, 48, 64, 128, 256, 512]) {
  const page = await browser.newPage({ viewport: { width: size, height: size }, deviceScaleFactor: 1 });
  await page.setContent(
    `<html><body style="margin:0;background:transparent">${svg.replace("<svg ", `<svg width="${size}" height="${size}" `)}</body></html>`,
  );
  await page.screenshot({ path: `packaging/icons/png/${size}.png`, omitBackground: true });
  await page.close();
}
await browser.close();
copyFileSync("packaging/icons/png/32.png", "app/src-tauri/icons/32x32.png");
copyFileSync("packaging/icons/png/128.png", "app/src-tauri/icons/128x128.png");
copyFileSync("packaging/icons/png/512.png", "app/src-tauri/icons/icon.png");
console.log("icons rendered");
