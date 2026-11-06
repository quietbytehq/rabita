<!-- SPDX-License-Identifier: Apache-2.0 -->
# app — Rabita Setup (wizard GUI)

- `src-tauri/` — Rust side of the Tauri 2 app. Unprivileged; talks to `rabita-helper` over D-Bus (`org.quietbyte.Rabita1`).
- `ui/` — the HTML/CSS/TypeScript wizard. Six screens: Welcome → Your organization → IT admin sign-in → Choose options → Setting up → Finish.

Milestone 1 contains stubs only. Tauri, the frontend toolchain and the D-Bus client are added in Milestone 3.
