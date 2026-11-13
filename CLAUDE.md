# CLAUDE.md — Rabita build brief

You are the developer for **Rabita**, an open-source Linux app by QuietByte (GitHub org `quietbytehq`, repo `quietbytehq/rabita`). The owner is not a developer: explain decisions in plain language, ask before anything risky, and keep him informed at each milestone.

## What Rabita is
A setup wizard that joins a Linux desktop to a Windows Active Directory domain (Windows Server 2012 R2 through current), so that:
1. Staff log in at the normal Linux login screen with their AD username and password (offline login cached).
2. Their mapped network drives (H:, S: etc.) mount automatically at login and appear in the file manager sidebar.
3. Their assigned AD/GPO printers are added automatically.

Audience: Windows users and school IT staff. It must feel like a Windows installer: Welcome → Your organization → IT admin sign-in → Choose options → Setting up → Finish, with ‹ Back / Next / Cancel.

"Rabita" means "connection" in Urdu/Arabic. Do not use "Active Directory", "Windows" or "Microsoft" in the product name, icon or package name (descriptive use in docs is fine).

## Non-negotiables
- **License:** Apache-2.0. Add LICENSE and SPDX headers.
- **Do not link Samba/SSSD libraries.** Call system tools (`realm`, `adcli`, `sssd`, `net`, `mount.cifs`, `lpadmin`, `klist`) as separate processes so our licence stays Apache-2.0.
- **Never store the admin password.** It is used once for the join and dropped from memory.
- **Never touch a real production domain while developing.** All testing happens in the test lab VMs.
- **Root access lives only in the helper** (see architecture). The GUI never runs as root.
- Every system change must be reversible: back up each config file before editing; "Leave domain" restores them.
- Releases and commits appear under `quietbytehq`, never a personal account.

## Architecture (three parts, one repo)
```
rabita/
  app/        Tauri 2 GUI (Rust + TypeScript/HTML/CSS) — the wizard + status screen
  helper/     rabita-helper: Rust root daemon on the system D-Bus, actions gated by polkit
  agent/      rabita-agent: Rust per-user service started at login (systemd user unit)
  backends/   per-distro logic inside helper (debian, fedora, suse, arch)
  packaging/  deb, rpm, AUR PKGBUILD, polkit policy, D-Bus config, systemd units
  docs/
```
1. **app (wizard GUI)** — unprivileged. Talks to helper over D-Bus (`org.quietbyte.Rabita1`). After joining, reopening the app shows a Status screen (connected?, domain controller, signed-in user, health checks, Refresh, Leave domain).
2. **helper (root)** — small and auditable. Methods: `DetectSystem`, `DiscoverDomain`, `Join`, `Leave`, `Status`, `ApplyOptions`. Each privileged method requires polkit auth (`org.quietbyte.rabita.manage`). Streams progress signals so the wizard's checklist updates live.
3. **agent (user session)** — runs at login. Reads the user's drive and printer assignments, mounts shares with Kerberos (`sec=krb5`), adds printers to CUPS, creates sidebar bookmarks named like `H: Home`. Quiet; optional tray icon.

## What the helper does during Join
1. Detect distro family, desktop, display manager (GDM/SDDM/LightDM).
2. Install packages via the native manager: realmd, sssd, adcli, krb5 client, samba-common-tools (as needed), cifs-utils, cups, keyutils.
3. Check time sync with the DC (Kerberos fails with clock skew; enable chrony/timesyncd against the DC if needed).
4. `realm join` with the admin creds (stdin, never argv), optional OU and computer name, pointed at the recommended (healthiest) DC so realm cannot pick one with a bad clock.
5. Configure SSSD: `use_fully_qualified_names = False`, `cache_credentials = True`, `fallback_homedir = /home/%u`, `ad_server = <recommended DC>, _srv_`. Never set `default_domain_suffix`: SSSD 2.10+ refuses to start when it is combined with `use_fully_qualified_names = False`. Remove `config_file_version`. Run `sssctl config-check` before restarting SSSD and stop with a clear error if it reports problems.
6. Enable home-dir creation on first login (pam_mkhomedir / oddjob-mkhomedir).
7. Hook into login:
   - Debian/Ubuntu: `pam-auth-update --enable sss mkhomedir`
   - Fedora/RHEL/Rocky: `authselect select sssd with-mkhomedir`
   - openSUSE: `pam-config -a --sss --mkhomedir`
   - Arch/Manjaro: edit `/etc/pam.d/system-auth` carefully (backup first) + nsswitch
8. Display manager: enable manual username entry ("Not listed?" / "Other user") — GDM `disable-user-list`, LightDM `greeter-show-manual-login=true`, SDDM theme setting.
9. Verify: `id <testuser>` resolves and a Kerberos ticket can be obtained. Report pass/fail per step.

If any step after the computer account is created fails, report the machine as "joined but not finished" and list the remaining steps. `ApplyOptions` (Finish setup) must be able to complete them.

## Drives and printers (agent)
- Sources, in order: user's `homeDirectory`/`homeDrive` from LDAP; GPO Preferences `Drives.xml` and `Printers.xml` from `\\<domain>\SYSVOL\...\Policies\{GUID}\User\Preferences\` for GPOs that apply to the user (respect basic item-level targeting by security group in v1; document what's unsupported).
- Mount points: `~/Network/H-Home` etc. Add GTK bookmarks (`~/.config/gtk-3.0/bookmarks`) and KDE Places entries.
- Printers: prefer driverless IPP Everywhere; else `smb://` via CUPS with Kerberos; flag "needs driver" in Status if no driver found.
- Unmount cleanly at logout.

## Supported distros
v1 target order: Ubuntu 22.04/24.04 + Debian 12 + Linux Mint → Fedora 40+/RHEL 9/Rocky 9 → openSUSE Leap/Tumbleweed → Arch/Manjaro.
Unsupported (show "Not supported yet" on Welcome, change nothing): immutable distros (Silverblue, Kinoite), NixOS, Alpine.

## Packaging
Native packages only: `.deb`, `.rpm`, AUR PKGBUILD. **No Snap/Flatpak/AppImage** — sandboxes can't change system login. Packages install the polkit policy, D-Bus service file, systemd units and a desktop entry "Rabita Setup".

## UI design (match the approved mockups)
- Window ~800×560, classic installer layout: left banner with logo and step list, content right, button bar bottom.
- Theme: dark navy window `#0F1730`, darker banner/title/footer `#0A1024`, panels `#111A36`, borders `#22305C`/`#3A4A80`, text `#E8ECF7`, muted `#A9B4D0`, accent mango `#FFB320` (dark text `#1A1300` on mango buttons). Font: Manrope.
- Logo: two circles joined by a line (two connected nodes).
- Motion: logo nodes pulse alternately and the joining line flows; slow rotating dotted rings in the banner; content fades/slides in per step; shimmer on the progress bar; pulsing dot on the active step; tick draws itself on Finish. Honour `prefers-reduced-motion`.
- Plain language everywhere ("Connect this computer to your organization"); a "Show details" link reveals the technical log.

## Build milestones (stop and report after each)
1. Repo scaffold, LICENSE, README, CI (build + clippy + tests) on GitHub Actions.
2. helper on Ubuntu: DetectSystem, DiscoverDomain, Join, Leave, Status via D-Bus + polkit. CLI test harness.
3. Wizard GUI wired to helper (all six screens) on Ubuntu.
4. agent: drive mapping + bookmarks.
5. agent: printers.
6. Status screen + Leave domain + restore of all backed-up configs.
7. Backends: Fedora/RHEL, then openSUSE, then Arch.
8. Packaging: .deb → .rpm → AUR; GitHub release v0.1.0.

## Test lab (owner's Proxmox)
- Windows Server DC VM with a test domain (e.g. `lab.quietbyte.test`), test users, 2–3 GPO drive maps, one shared printer, an OU for Linux PCs, a delegated "join computers" account.
- Client VMs: Ubuntu 24.04, Fedora, Arch — snapshot before every join test.
- Write `docs/testing.md` with the manual test checklist per milestone.

## Working style
- Small commits with clear messages; open a PR per milestone.
- Before running anything that changes the owner's own machine, ask.
- When something can't be done reliably, say so and propose the simplest alternative.
