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

## D-Bus interface (planned, Milestone 2)

`org.quietbyte.Rabita1` at `/org/quietbyte/Rabita1`:

| Method           | Needs polkit | Purpose                                            |
|------------------|--------------|----------------------------------------------------|
| `DetectSystem`   | no           | Distro family, desktop, display manager            |
| `DiscoverDomain` | no           | Find DCs for a domain name (DNS SRV)               |
| `Join`           | yes          | Install packages, `realm join`, configure SSSD/PAM |
| `Leave`          | yes          | `realm leave`, restore backed-up configs           |
| `Status`         | no           | Joined?, DC, signed-in user, health checks         |
| `ApplyOptions`   | yes          | Re-apply wizard options without re-joining         |

Progress is streamed as a `Progress(step, state, message)` signal so the
wizard's checklist updates live.
