<!-- SPDX-License-Identifier: Apache-2.0 -->
# D-Bus API: `org.quietbyte.Rabita1`

The helper owns the well-known name `org.quietbyte.Rabita1` on the **system
bus** and exports one object at `/org/quietbyte/Rabita1` with the interface
`org.quietbyte.Rabita1`. The wizard and `rabitactl` are ordinary unprivileged
clients.

Structured data travels as **JSON strings**. The shapes are the Rust types in
`helper/src/types.rs` (serde, snake_case fields), so the TypeScript wizard can
consume them directly and the interface stays stable while the structures grow.

## Methods

| Method | In | Out | polkit | Notes |
|---|---|---|---|---|
| `DetectSystem()` | – | `s` JSON `SystemInfo` | no | Distro, family, supported?, display manager, host name, joined domain |
| `DiscoverDomain(domain s)` | domain name | `s` JSON `DiscoveryResult` | no | DNS SRV lookup of `_ldap._tcp.dc._msdcs.<domain>` (plus the client site's `_sites` record when `adcli` can tell the site), parallel TCP/SNTP probes, ranking and warnings |
| `Status()` | – | `s` JSON `StatusReport` | no | Membership, sssd config/service, keytab, Kerberos ticket, DC reachability, clock, home dirs, user lookup |
| `Join(request s, password s)` | JSON `JoinRequest`, admin password | `u` job id | **yes** | Returns at once; progress via signals |
| `Leave(request s, password s)` | JSON `LeaveRequest`, password (empty unless removing the computer account) | `u` job id | **yes** | Restores every backed-up file |
| `ApplyOptions(options s)` | JSON `JoinOptions` | `u` job id | **yes** | Re-applies who-can-sign-in, home-folder and login-screen options |

Privileged methods check `org.quietbyte.rabita.manage` with
`AllowUserInteraction`, so the desktop shows the normal authentication dialog.
Root is always authorised. A refusal returns
`org.freedesktop.DBus.Error.AccessDenied`; if polkit cannot be reached the
helper fails closed with `org.freedesktop.DBus.Error.AuthFailed`.

Only one job runs at a time; a second `Join`/`Leave` while busy returns
`org.freedesktop.DBus.Error.Failed`.

## Signals

| Signal | Args | Meaning |
|---|---|---|
| `Progress` | `job u, step s, state s, message s` | A step changed state. `state` ∈ `pending`, `running`, `done`, `failed`, `skipped` |
| `LogLine` | `job u, line s` | A line for the technical log ("Show details") |
| `JobFinished` | `job u, ok b, result_json s` | Job ended; `result_json` is a `JobResult` with every step's final state |

Subscribe to the signals **before** calling `Join`/`Leave` so no early step is
missed.

## Properties

| Property | Type | Meaning |
|---|---|---|
| `Version` | `s` | Helper version |
| `Busy` | `b` | A job is running |

## Join steps (ids used in `Progress`)

`detect`, `packages`, `time`, `join`, `sssd`, `access`, `home`, `display`, `verify`.

## Discovery result (`DiscoveryResult`)

Controllers are listed **best first**: DCs in the client's own AD site, then
DCs whose clock agrees with the majority of DCs, then smallest clock offset,
then SRV priority/weight; unreachable DCs last. Fields beyond the obvious:

| Field | Meaning |
|---|---|
| `client_site` | The computer's AD site, when `adcli info` could report it (needs `adcli` installed; always true during a join, not on a fresh machine) |
| `recommended` | Host of the healthiest DC; the join uses it for time sync. `null` when nothing is reachable |
| `majority_clock_offset_secs` | How far this computer's clock is from the time most DCs agree on. A DC that disagrees with this majority is never used to set the time |
| `warnings` | Plain-language summary: unreachable DCs ("possibly stale DNS records") and DCs more than 60 s off |
| per DC `in_client_site`, `recommended`, `warning` | Same, per controller |

`Status` reports the same in two checks: `dc` (is the configured or best DC
reachable) and `dc-health` (warnings, with `warning: true` when any).
`HealthCheck.warning` marks a check that passed but deserves attention.

## Options (`JoinOptions`, also the body of `ApplyOptions`)

```json
{
  "manual_login": true,
  "create_home": true,
  "offline_login": true,
  "login_access": { "mode": "all" }
}
```

`login_access` is either `{"mode":"all"}` (every domain user may sign in,
the default) or `{"mode":"groups","groups":["Linux Users","Staff"]}` (only
members of those AD groups). Group mode runs `realm deny --all` followed by
`realm permit -g <groups…>`, so the list is exact each time it is applied; a
group list that is empty after trimming is rejected before anything changes.
`Status` reports the current setting in the `login-access` check.

## Leave steps

`leave`, `restore`, `login`, `display`, `finish`.

## Password handling

The password is a plain string argument on the bus (the same approach
`realmd` uses). The helper wraps it in a zeroising buffer, writes it once to
`realm join`'s standard input, and drops it. It never appears on a command
line, in a log line or in a signal.

## Example with `busctl`

```bash
busctl call org.quietbyte.Rabita1 /org/quietbyte/Rabita1 org.quietbyte.Rabita1 DetectSystem
```

```bash
busctl monitor org.quietbyte.Rabita1
```
