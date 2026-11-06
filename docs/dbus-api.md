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
| `DiscoverDomain(domain s)` | domain name | `s` JSON `DiscoveryResult` | no | DNS SRV lookup of `_ldap._tcp.dc._msdcs.<domain>`, TCP reachability, clock offset via SNTP |
| `Status()` | – | `s` JSON `StatusReport` | no | Membership, sssd config/service, keytab, Kerberos ticket, DC reachability, clock, home dirs, user lookup |
| `Join(request s, password s)` | JSON `JoinRequest`, admin password | `u` job id | **yes** | Returns at once; progress via signals |
| `Leave(request s, password s)` | JSON `LeaveRequest`, password (empty unless removing the computer account) | `u` job id | **yes** | Restores every backed-up file |
| `ApplyOptions(options s)` | JSON `JoinOptions` | `u` job id | **yes** | Re-applies home-folder and login-screen options |

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

`detect`, `packages`, `time`, `join`, `sssd`, `home`, `display`, `verify`.

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
