<!-- SPDX-License-Identifier: Apache-2.0 -->
# Architecture

Rabita is three programs in one repository. Only one of them runs as root.

```
 ┌──────────────────────┐   D-Bus (system bus)    ┌──────────────────────┐
 │  rabita-app          │ ──────────────────────▶ │  rabita-helper       │
 │  wizard + status UI  │   org.quietbyte.Rabita1 │  root, polkit-gated  │
 │  runs as the user    │ ◀────────────────────── │  realm/adcli/sssd    │
 └──────────────────────┘   progress signals      └──────────────────────┘

 ┌──────────────────────┐
 │  rabita-agent        │  started by systemd --user at login
 │  runs as the user    │  mount.cifs (sec=krb5), lpadmin, bookmarks
 └──────────────────────┘
```

## Rules the code must keep

- **No Samba or SSSD libraries are linked.** The helper and agent run the
  system tools (`realm`, `adcli`, `net`, `mount.cifs`, `lpadmin`, `klist`)
  as separate processes. This keeps Rabita Apache-2.0.
- **The admin password is never stored.** It is passed to `realm join` on
  stdin once and then dropped from memory.
- **Every change is reversible.** The helper backs up each config file
  before editing; "Leave domain" restores them.
- **Root only in the helper.** The GUI and agent never gain privileges.

## Crates

| Crate             | Path             | Binary          | Role                          |
|-------------------|------------------|-----------------|-------------------------------|
| `rabita-helper`   | `helper/`        | `rabita-helper` | Root D-Bus daemon             |
| `rabita-agent`    | `agent/`         | `rabita-agent`  | Per-user login service        |
| `rabita-app`      | `app/src-tauri/` | `rabita-app`    | Tauri 2 wizard                |
| `rabita-backends` | `backends/`      | (library)       | Distro-specific steps         |

## D-Bus interface (Milestone 2)

`org.quietbyte.Rabita1` at `/org/quietbyte/Rabita1`, documented in full in
[dbus-api.md](dbus-api.md):

| Method           | Needs polkit | Purpose                                            |
|------------------|--------------|----------------------------------------------------|
| `DetectSystem`   | no           | Distro family, display manager, joined?            |
| `DiscoverDomain` | no           | Find DCs for a domain name (DNS SRV), probe them   |
| `Join`           | yes          | Install packages, `realm join`, configure SSSD/PAM |
| `Leave`          | yes          | `realm leave`, restore backed-up configs           |
| `Status`         | no           | Joined?, DC, health checks                         |
| `ApplyOptions`   | yes          | Re-apply wizard options (who can sign in, home folders, login screen) |

`Join`, `Leave` and `ApplyOptions` return a job id immediately and stream
`Progress(job, step, state, message)`, `LogLine(job, line)` and
`JobFinished(job, ok, result_json)` signals so the wizard's checklist updates
live and no D-Bus call waits on `apt-get`.

## How the helper is built (so it stays auditable)

- **Backends plan, the helper executes.** `rabita-backends` turns "install
  packages" or "show a username box on the login screen" into a list of
  `Action`s (run a command, write a file, remove a file). It never touches the
  system. The helper runs the actions, backing up each file first.
- **Every process goes through a `Runner`.** Unit tests use a scripted
  `FakeRunner`, so `cargo test` never runs `realm`, `apt-get` or `systemctl`.
- **All paths hang off a root.** Tests point the helper at a temporary
  directory and exercise the real backup and restore code.
- **State lives in `/var/lib/rabita/state.json`** (root only) with the list of
  backups in `/var/lib/rabita/backups/<timestamp>/`. "Leave domain" walks that
  list in reverse.

## Lab tooling

- `rabitactl` — CLI test harness; talks to the helper over D-Bus exactly like
  the wizard. `--session` targets a helper started with `rabita-helper
  --session`; `--direct` runs the library in-process (lab debugging only).
- `scripts/dev-install.sh` / `dev-uninstall.sh` — install a development build
  with its polkit and D-Bus files on a lab VM.
