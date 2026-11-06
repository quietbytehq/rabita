<!-- SPDX-License-Identifier: Apache-2.0 -->
# packaging

Native packages only (`.deb`, `.rpm`, AUR). No Snap, Flatpak or AppImage: sandboxes cannot change how the system logs in.

| Folder     | Contents                                                              | Milestone |
|------------|-----------------------------------------------------------------------|-----------|
| `polkit/`  | `org.quietbyte.rabita.policy` — the one action the helper checks       | 1 (done)  |
| `dbus/`    | System-bus policy and activation file for `org.quietbyte.Rabita1`     | 1 (done)  |
| `systemd/` | `rabita-helper.service` (system) and `rabita-agent.service` (user)    | 1 (done)  |
| `desktop/` | "Rabita Setup" launcher entry                                          | 1 (done)  |
| `deb/`     | Debian/Ubuntu packaging                                                | 8         |
| `rpm/`     | Fedora/RHEL/openSUSE spec                                              | 8         |
| `aur/`     | PKGBUILD for Arch/Manjaro                                              | 8         |

Install locations (all packages):

- `/usr/libexec/rabita-helper`, `/usr/bin/rabita-agent`, `/usr/bin/rabita-app`
- `/usr/share/polkit-1/actions/org.quietbyte.rabita.policy`
- `/usr/share/dbus-1/system.d/org.quietbyte.Rabita1.conf`
- `/usr/share/dbus-1/system-services/org.quietbyte.Rabita1.service`
- `/usr/lib/systemd/system/rabita-helper.service`
- `/usr/lib/systemd/user/rabita-agent.service`
- `/usr/share/applications/org.quietbyte.Rabita.desktop`
