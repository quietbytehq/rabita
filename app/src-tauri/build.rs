// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! Embeds the wizard's web files and Tauri configuration. The front end
//! must be built first: `npm --prefix app/ui ci && npm --prefix app/ui run build`.

fn main() {
    tauri_build::build();
}
