<!-- SPDX-License-Identifier: Apache-2.0 -->
# app — Rabita Setup (wizard GUI)

The installer-style wizard: Welcome → Your organization → IT admin sign-in →
Choose options → Setting up → Finish. It never runs as root; it asks
`rabita-helper` to do the work over D-Bus, and polkit shows the password
dialog when the helper needs permission.

- `ui/` — the web page shown in the window.
  - `src/` TypeScript: `types.ts` (the helper's JSON), `logic.ts` (wizard
    rules and wording, unit-tested), `api.ts` (calls into the Rust side),
    `main.ts` (the screens).
  - `public/` HTML, CSS and the bundled Manrope font (SIL OFL 1.1, see
    `public/fonts/OFL.txt`). Nothing is loaded from the internet.
  - `test/` unit tests for `logic.ts` (`node --test`).
  - `dev/` development only, never shipped: a mock of the helper and a
    script that clicks through every screen in Chromium and saves
    `docs/screenshots/`.
- `src-tauri/` — the Rust side (Tauri 2). `bridge.rs` forwards commands to
  `org.quietbyte.Rabita1` and turns the helper's progress signals into
  events for the page. `tauri.conf.json` sets the 800 × 560 window and a
  strict content-security policy.

## Build and run

```bash
npm --prefix app/ui ci
```

```bash
npm --prefix app/ui run build
```

```bash
cargo build -p rabita-app
```

The page must be built before the Rust side: Tauri embeds `ui/dist` into the
binary.

To try it without installing anything system-wide, run a helper on your
session bus in one terminal and the app in another:

```bash
cargo run -p rabita-helper -- --session
```

```bash
RABITA_BUS=session cargo run -p rabita-app
```

Detecting and finding a domain work like this; joining needs the real
system helper and polkit (see `docs/testing.md`).

## Tests

```bash
npm --prefix app/ui test
```

```bash
NODE_PATH=$(npm root -g) node app/ui/dev/screenshots.mjs
```

The second needs Playwright with Chromium; it checks the navigation rules,
the "joined but not finished" path, the wrong-password path, reduced motion,
and that the password never goes into the join request.

## Package

```bash
packaging/deb/build-deb.sh
```

builds `target/deb/rabita_<version>_<arch>.deb` with the wizard, the helper,
`rabitactl`, and the polkit, D-Bus, systemd and desktop files.
