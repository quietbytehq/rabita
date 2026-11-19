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
| `Leave(request s, password s)` | JSON `LeaveRequest`, password (empty unless removing the computer account) | `u` job id | **yes** | Leaves (removing the computer account if asked, else locally), then restores every backed-up file, the login rules, the login screen, the DNS setting and the computer name. On a computer that is not joined it undoes what an unfinished attempt left (DNS, name) |
| `FindDnsServers(domain s)` | domain name | `s` JSON `DnsSearch` | no | Looks at the gateway, then the rest of the active connection's network (at most a /24), for DNS servers that answer for `_ldap._tcp.dc._msdcs.<domain>`. Read-only |
| `SetDnsServer(domain s, server s)` | domain, IP address | `s` JSON `DnsResult` | **yes** | Checks that `server` knows the domain **before** changing anything, records the active NetworkManager connection's DNS settings, sets the new server, and waits for the domain to resolve. Leave (or `RestoreDns`) puts the old setting back |
| `RestoreDns()` | – | `s` JSON `DnsResult` | **yes** | Puts back the DNS setting `SetDnsServer` changed. Nothing to do is not an error |
| `ApplyOptions(options s)` | JSON `JoinOptions` | `u` job id | **yes** | Re-applies who-can-sign-in, home-folder and login-screen options. On a computer that is joined but not finished it runs every post-join step to finish setup (see below) |

Privileged methods check `org.quietbyte.rabita.manage` with
`AllowUserInteraction`, so the desktop shows the normal authentication dialog.
Root is always authorised. A refusal returns
`org.freedesktop.DBus.Error.AccessDenied`; if polkit cannot be reached the
helper fails closed with `org.freedesktop.DBus.Error.AuthFailed`.

If the desktop's password window is left unanswered for 5 minutes, the
helper closes it (polkit `CancelCheckAuthorization`) and refuses with
`AccessDenied` (*Nobody answered the password window within 5 minutes…*).

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

`detect`, `packages`, `time`, `name`, `join`, `sssd`, `access`, `home`, `display`, `verify`.

`name` renames the computer (`hostnamectl set-hostname`, plus its line in
`/etc/hosts`, both backed up) when `JoinRequest.new_hostname` is set, and is
`skipped` otherwise. Active Directory computer names can have at most 15
characters, only letters, digits and hyphens, no hyphen at either end, and
not only digits. `DetectSystem` reports a current name that breaks these
rules as `hostname_problem`, with a `suggested_hostname`; `Join` then refuses
unless `new_hostname` is given. The original name is recorded, and Leave
restores it.

## Computer name and DNS fields

- `JoinRequest.new_hostname` (`string|null`): rename to this before joining.
- `SystemInfo.hostname_problem`, `SystemInfo.suggested_hostname`: see above.
- `SystemInfo.dns_change` (`DnsChange|null`): the DNS change Rabita made and
  the original values it will restore: `connection_uuid`, `connection_name`,
  `device`, `server`, `domain`, `original_dns`, `original_dns_search`,
  `original_ignore_auto_dns`, `original_ipv6_ignore_auto_dns`.
- `SystemInfo.restore_pending` (`bool`): the computer is not joined but an
  unfinished attempt left a DNS change or a new name. The wizard offers
  **Undo changes**, which is `Leave` with `remove_computer_account: false`.
- `DnsSearch`: `connection_name`, `device`, `candidates` (IP addresses that
  know the domain), `scanned` (how many were asked), `problem` (plain-text
  reason when it could not look, for example without NetworkManager).
- `DnsResult`: `ok`, `message` (plain text, shown as is).

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

## Joined but not finished

`Join` creates the computer account in step `join`. The steps after it,
`sssd`, `access`, `home`, `display` and `verify`, can still fail. When one
does:

- `JobResult.joined_not_finished` is `true`, `remaining_steps` lists the ids
  still to run, and `summary` says in plain words that the computer is
  joined but not finished and what to do. Steps that never ran are reported
  as `skipped` with *Not run yet. Finish setup will run it.*
- The helper's state file records the same. A second `Join` refuses and
  points to Finish setup.
- `Status` returns `setup_finished: false`, the same `remaining_steps`, and a
  failing `setup` check. It also returns `options`, the choices saved at
  join.

**Finish setup** is `ApplyOptions` with those saved `options`. That is what
`rabitactl finish` sends, so finishing never changes a choice; for example, a
group restriction stays. `ApplyOptions` runs all five post-join steps, not
only the option steps, when any of these holds:

- setup is not finished;
- the computer was joined by an older Rabita (`setup_finished` is `null`);
- `sssd.conf` still contains `default_domain_suffix`, which stops SSSD 2.10+
  from starting.

All five steps are safe to repeat. On success `summary` is *Setup finished.
This computer is fully connected to …*.

The `sssd` step rewrites `sssd.conf` and runs `sssctl config-check`. If the
checker reports problems, the step fails with them and SSSD is **not**
restarted. If SSSD then fails to start, the last journal lines go to the log.

The `join` step passes the recommended DC's host name to `realm join` instead
of the domain name, so realmd enrols through that server. If realm cannot use
it, the step retries with the domain name and logs a warning. The step
message names the DC that adcli reports it used.

## Leave

`LeaveRequest`: `{ "remove_computer_account": bool, "admin_user": string|null }`.
The password argument is empty unless `remove_computer_account` is true.

Steps: `leave`, `restore`, `login`, `display`, `dns`, `name`, `finish`.

- `leave`: `realm leave`, with `--remove` and the admin account when asked
  (password on standard input). If the credentials are refused, the job
  stops and nothing else changes. If the domain cannot be reached, the
  helper logs a warning, leaves on this computer only, and sets
  `JobResult.left_locally_only: true`; the summary tells the technician to
  delete the computer account in Active Directory Users and Computers.
- `restore` to `name` are best effort: each one runs even if an earlier one
  failed, and failures are listed in the summary (*some settings could not
  be put back*). The state file is kept in that case so running Leave again
  retries them.
- `finish` clears the state file when everything was put back.

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
