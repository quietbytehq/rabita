<!-- SPDX-License-Identifier: Apache-2.0 -->
# Contributing to Rabita

Thanks for helping. A few ground rules keep the project safe and its licence clean.

## Ground rules

- **Apache-2.0 only.** Every source file starts with `SPDX-License-Identifier: Apache-2.0`. `scripts/check-spdx.sh` enforces this in CI.
- **Do not link Samba or SSSD libraries.** Call `realm`, `adcli`, `net`, `mount.cifs`, `lpadmin` and friends as separate processes.
- **Never store the admin password.** It is used once for the join and then dropped.
- **Root lives only in `helper/`.** The GUI and agent must never run privileged.
- **Every system change is reversible.** Back up a config file before editing it.
- **Test only in the lab.** Never point a development build at a production domain.

## Local checks

```bash
npm --prefix app/ui ci && npm --prefix app/ui test
```

```bash
cargo fmt --all --check
```

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

```bash
cargo test --workspace
```

```bash
scripts/check-spdx.sh
```

```bash
scripts/dbus-smoke.sh
```

## Workflow

Small commits with clear messages. One pull request per milestone (see `CLAUDE.md`).
