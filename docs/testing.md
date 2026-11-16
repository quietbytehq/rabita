<!-- SPDX-License-Identifier: Apache-2.0 -->
# Manual test checklist

All testing happens in the lab VMs. **Never run a join against a real
production domain.** Snapshot every client VM before each join test.

## Lab

- Windows Server DC with test domain `lab.quietbyte.test`
- Test users, 2–3 GPO drive maps, one shared printer
- An OU for Linux PCs and a delegated "join computers" account
- Client VMs: Ubuntu 24.04, Fedora, Arch

## Milestone 1 — scaffold

- [ ] `cargo build --workspace` succeeds on a clean checkout
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean
- [ ] `cargo fmt --all --check` is clean
- [ ] `scripts/check-spdx.sh` reports no missing headers
- [ ] GitHub Actions CI is green on the pull request
- [ ] `scripts/dbus-smoke.sh` passes (private session bus, no root)

## Milestone 2 — helper on Ubuntu

What this proves: the root helper can detect the system, find the lab domain,
join it with the delegated account, configure sign-in, verify it, report
status, and leave again with every file restored. All of it over D-Bus with
polkit, exactly as the wizard will use it.

### Before you start

- Lab DC up with domain `lab.quietbyte.test`, a delegated "join computers"
  account (below: `joiner`), an OU for Linux PCs (below:
  `OU=Linux PCs,DC=lab,DC=quietbyte,DC=test`) and a normal test user (below:
  `amna`). Substitute your real names.
- Ubuntu 24.04 desktop VM (GNOME/GDM), with **DNS pointing at the DC** and a
  clock roughly right. Check: `resolvectl status` shows the DC's IP as DNS
  server.
- **Take a VM snapshot now.** Restore it between runs.

### 1. Install the development build on the VM

```bash
sudo apt update && sudo apt install -y git build-essential pkg-config curl
```

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && source "$HOME/.cargo/env"
```

```bash
git clone https://github.com/quietbytehq/rabita.git && cd rabita && git checkout <branch-under-test>
```

```bash
scripts/dev-install.sh
```

Expected: ends with "Installed. Next: …". Check the policy files landed:

```bash
ls -l /usr/share/polkit-1/actions/org.quietbyte.rabita.policy /usr/share/dbus-1/system.d/org.quietbyte.Rabita1.conf
```

### 2. Start the helper and talk to it

Terminal 1 (leave it open; this is the helper's log):

```bash
sudo /usr/local/libexec/rabita-helper
```

Expected: `rabita-helper 0.1.0-dev: serving org.quietbyte.Rabita1 on the system bus`.

Terminal 2 (your normal desktop user, **not** root):

```bash
rabitactl version
```

```bash
rabitactl detect
```

- [ ] `supported: true`, `family: "debian"`, `display_manager: "gdm"`,
      `joined_domain: null`, `realmd_installed: false` on a fresh VM.

```bash
rabitactl discover lab.quietbyte.test
```

- [ ] `found: true`; controllers are listed best first and exactly one has
      `recommended: true`. `recommended` names it.
- [ ] `majority_clock_offset_secs` is close to 0 (anything under 120 is fine).
- [ ] On a domain with stale DNS records or wrong clocks: unreachable DCs
      carry a `warning` mentioning *stale DNS record*, DCs more than 60 s off
      carry a clock warning, and `warnings` summarises both. A DC that
      disagrees with the majority is never the recommended one.
- [ ] `client_site` is `null` before `adcli` is installed and filled in after
      a join (re-run `rabitactl discover` after step 4); DCs in that site then
      show `in_client_site: true` and sort first.
- [ ] `rabitactl discover LAB` says to enter the full domain name.
- [ ] `rabitactl discover nowhere.invalid` says no domain controllers found.

```bash
rabitactl status
```

- [ ] `joined: false` with a single failing "Domain membership" check.

### 3. Negative test first: wrong password

```bash
rabitactl join lab.quietbyte.test --user joiner --ou "OU=Linux PCs,DC=lab,DC=quietbyte,DC=test" --verify-user amna
```

- [ ] A polkit dialog asks you to authenticate (your own sudo password). Approve it.
- [ ] At "Password for joiner:" type a **wrong** password.
- [ ] Steps run: packages get installed (first time takes a few minutes), the
      domain is found, then "Join the domain" fails with
      *The administrator username or password was not accepted.*
- [ ] `rabitactl status` still says `joined: false`; `realm list` prints nothing.

### 4. Join for real

Run the same command with the correct password.

- [ ] All nine steps end `OK` or `--` (skipped), then `SUCCESS` and
      *This computer is now connected to lab.quietbyte.test.*
- [ ] The "Choose who can sign in" step says *All domain users*.
- [ ] Terminal 1 shows no errors.
- [ ] The "Find the domain and check the clock" step names the recommended
      DC and, if the domain has stale records or bad clocks, says how many
      warnings there are; the details log lists them.
- [ ] The "Join the domain" step says *Joined lab.quietbyte.test using
      domain controller X*, where X is the `recommended` DC from
      `rabitactl discover`, not whichever DC realm would have picked.
- [ ] Verify step reports *Kerberos ticket OK; user 'amna' found*.

Checks from the shell:

```bash
realm list
```

```bash
id amna
```

```bash
rabitactl status
```

- [ ] `realm list` shows `configured: kerberos-member`.
- [ ] `id amna` prints a uid and groups (no `@lab.quietbyte.test` suffix needed).
- [ ] Every check in `rabitactl status` has `ok: true`; `domain_controller`
      is filled in; `joined_at` is now. The "Domain controller health" check
      shows `warning: true` and lists stale records or bad clocks if the
      domain has any.
- [ ] The "Setup" check in `rabitactl status` says *Finished*.
- [ ] `sudo cat /etc/sssd/sssd.conf` contains
      `use_fully_qualified_names = False`, `fallback_homedir = /home/%u`,
      `cache_credentials = True` and `ad_server = <recommended DC>, _srv_`.
      It does **not** contain `default_domain_suffix` or `config_file_version`.
- [ ] `sudo sssctl config-check` prints *Issues identified by validators: 0*
      and `systemctl is-active sssd` prints `active` (tested on SSSD 2.12,
      which refuses to start with the old settings).
- [ ] `sudo ls /var/lib/rabita/backups/*/` lists the backed-up files
      (`etc__krb5.conf`, `etc__pam.d__common-session`, …).
- [ ] On the DC: the computer object exists in the Linux PCs OU.

### 4b. Joined but not finished, then Finish setup

This forces a failure after the computer account exists, to check that
Rabita says so and can finish later. Start from a fresh snapshot.

Install the packages first, so the forced failure does not hit the package
step (installing `libpam-sss` runs `pam-auth-update` itself):

```bash
sudo apt-get install -y realmd sssd sssd-tools adcli krb5-user libnss-sss libpam-sss samba-common-bin cifs-utils keyutils cups
```

```bash
sudo chmod -x /usr/sbin/pam-auth-update
```

```bash
rabitactl join lab.quietbyte.test --user joiner --verify-user amna
```

- [ ] The run ends with `JOINED BUT NOT FINISHED` and a summary that starts
      *This computer is joined to lab.quietbyte.test but not finished*, says
      what failed, and lists the remaining steps: *Create home folders on
      first login, Show a username box on the login screen, Check that it
      works*.
- [ ] `rabitactl status`: the "Setup" check fails with *Joined but not
      finished. Still to do: …*.
- [ ] Running the join again refuses with *… already joined to
      lab.quietbyte.test but not finished. Run Finish setup …*.

Put the tool back and finish:

```bash
sudo chmod +x /usr/sbin/pam-auth-update
```

```bash
rabitactl finish
```

- [ ] `SUCCESS` and *Setup finished. This computer is fully connected to
      lab.quietbyte.test.* All five steps (sign-in, who can sign in, home
      folders, login screen, check) ran.
- [ ] `rabitactl status`: "Setup" says *Finished*.
- [ ] Logging in as `amna` creates `/home/amna`.
- [ ] `rabitactl finish` uses the choices made at join: if you joined with
      `--allow-group`, the group list is unchanged afterwards.

### 4c. Repair a computer joined by an earlier build

On the Ubuntu 26.04 VM that was joined before this fix (SSSD not starting,
no home folder), without restoring the snapshot:

```bash
rabitactl status
```

- [ ] The "Sign-in configuration" check fails with *sssd.conf sets
      default_domain_suffix, which stops SSSD 2.10 and newer from starting*,
      and "Setup" warns *Joined by an earlier version of Rabita*.

```bash
rabitactl finish
```

- [ ] `SUCCESS`; `default_domain_suffix` and `config_file_version` are gone
      from `/etc/sssd/sssd.conf`; `systemctl is-active sssd` prints `active`;
      the next domain login gets a home folder.

### 5. Log in as a domain user

Log out. On the GDM login screen:

- [ ] A "Not listed?" link (or a plain username box) is shown.
- [ ] Type `amna` and the AD password. The desktop opens.
- [ ] A terminal shows `echo $HOME` = `/home/amna` and `klist` shows a ticket
      for `amna@LAB.QUIETBYTE.TEST`.
- [ ] Log out, **disconnect the VM's network**, log in as `amna` again
      (offline login works from cache). Reconnect the network afterwards.

If the "Not listed?" link does not appear, note it and tell the developer
(the GDM dconf profile may need a different path on this Ubuntu release).
Workaround for the test: press the username box after clicking any user name
and use the "Login" entry, or test on a LightDM session.

### 5b. Restrict sign-in to groups

On the DC, create a security group `Linux Users` and make sure `amna` is
**not** in it yet. Pick a second test user who **is** in it (below: `bilal`).

```bash
rabitactl apply-options --allow-group "Linux Users"
```

- [ ] `SUCCESS`; the "Choose who can sign in" step says
      *Only these groups: Linux Users*.
- [ ] `rabitactl status` shows the "Who can sign in" check as
      *Only these groups: Linux Users*.
- [ ] `sudo realm list` shows `login-policy: allow-permitted-logins` and
      `permitted-groups: Linux Users`.
- [ ] Log out. Logging in as `amna` is **refused** (GDM shows an error; the
      helper log in terminal 1 is unaffected). Logging in as `bilal` works.
- [ ] Add `amna` to `Linux Users` on the DC, then on the VM run
      `sudo sss_cache -E` and wait a minute: `amna` can log in again.

Switch back to everyone:

```bash
rabitactl apply-options
```

- [ ] The step says *All domain users*; `rabitactl status` agrees; `sudo realm
      list` shows `login-policy: allow-realm-logins`.
- [ ] A join can also start restricted:
      `rabitactl join lab.quietbyte.test --user joiner --allow-group "Linux Users" --allow-group Staff`
      (run this variant on a fresh snapshot if you want to confirm it; the
      "Choose who can sign in" step should say *Only these groups: Linux Users, Staff*).
- [ ] `rabitactl apply-options --allow-group " "` is rejected with *Enter at
      least one group, or choose 'All domain users'.* and nothing changes.

### 6. Options and idempotence

```bash
rabitactl apply-options --manual-login false
```

- [ ] `SUCCESS`; after `sudo systemctl restart gdm3` the user list is back.

```bash
rabitactl apply-options --manual-login true
```

```bash
rabitactl join lab.quietbyte.test --user joiner
```

- [ ] The second join refuses: *This computer is already connected to … Use
      'Leave domain' first.* Nothing changes.

### 7. Leave

```bash
rabitactl leave
```

- [ ] All five steps `OK`/`--`, then *This computer has left lab.quietbyte.test.
      Original settings are restored.*
- [ ] `realm list` prints nothing; `id amna` fails.
- [ ] `/etc/sssd/sssd.conf` is gone (it did not exist before the join);
      `/etc/krb5.conf` matches the snapshot's version.
- [ ] `sudo ls /var/lib/rabita/` shows `last-leave.json` but no `state.json`.
- [ ] After `sudo systemctl restart gdm3` the login screen is back to the user list.
- [ ] Optional: `rabitactl leave --remove --user joiner` on a joined machine
      also deletes the computer object on the DC.

### 8. polkit and the cancel path

As the desktop user, run a join and press **Cancel** in the polkit dialog.

- [ ] `rabitactl` reports *Authentication was cancelled.* and nothing runs.

Over SSH with no desktop agent, a plain user gets *You are not allowed to
change this computer's organization connection.* while `sudo rabitactl join …`
works (root is always authorised).

### 9. Clean up

```bash
scripts/dev-uninstall.sh
```

Then restore the VM snapshot.

### Fallbacks

- `rabitactl --direct …` runs the helper code in-process without D-Bus or
  polkit (`sudo` needed for join/leave). Use it only to tell a D-Bus problem
  apart from a join problem.
- `rabitactl --json …` prints the full result as JSON for bug reports.
- `busctl monitor org.quietbyte.Rabita1` shows the raw signals.

### Known limits in this milestone

- Rabita joins by giving realm the recommended DC's host name, so realmd
  enrols through that server. If realm cannot use it by name, Rabita logs a
  warning and retries with the domain name; the "Join the domain" step then
  says which DC realm used.

- SDDM (KDE) gets no login-screen change yet; most SDDM themes already offer
  "Other".
- Who may sign in is now a choice (all domain users, or only listed groups).
  Separately, `ad_gpo_access_control` is set to `permissive`, so Windows
  "Allow log on locally" GPOs are not enforced on Linux machines; the group
  list above is the way to restrict access.
- Fedora, openSUSE and Arch backends are placeholders (Milestone 7).

## Milestone 3 — wizard GUI and .deb

What this proves: a normal user can install Rabita by double-clicking a
`.deb`, open **Rabita Setup** from the app menu, and connect the computer
through the six screens without a terminal. Screenshots of every screen are
in `docs/screenshots/`.

Start from a fresh Ubuntu 24.04 (or 26.04) snapshot with GNOME, DNS pointing
at the DC, as for Milestone 2. Do **not** install the Milestone 2 dev build
on the same snapshot.

### 1. Get the package

- [ ] On the pull request on GitHub, open the **Checks** tab, choose the
      latest **CI** run, and download the **rabita-deb** artifact (a zip).
      Unzip it to get `rabita_0.1.0~dev_amd64.deb`.

(Or build it yourself on an Ubuntu 22.04 machine with
`packaging/deb/build-deb.sh`; the result lands in `target/deb/`.)

### 2. Install with a double-click

- [ ] Copy the `.deb` to the VM and double-click it in Files. The software
      app opens (App Center on Ubuntu 24.04+, Software Manager / GDebi on
      Mint, Software on Debian) and shows **Rabita Setup** with its logo
      and description. Choose **Install** and enter your password.
- [ ] If the software app refuses or does nothing, that is a bug: note
      what it said and take a screenshot.

- [ ] **Rabita Setup** appears in the app menu (search "domain" or
      "Rabita") with the two-nodes logo.

### 3. Look and feel

Open **Rabita Setup** from the menu.

- [ ] An 800 × 560 window titled *Rabita Setup*: dark navy, mango buttons,
      Manrope font, left banner with the logo and the six steps, buttons
      at the bottom (‹ Back, Next ›, Cancel).
- [ ] Motion: the logo's two nodes pulse in turn and the line between them
      flows; dotted rings turn slowly in the banner; the active step's dot
      pulses; each screen slides in.
- [ ] Turn on **Settings → Accessibility → Reduce animation**, close and
      reopen Rabita Setup: nothing moves. Turn it back off.
- [ ] `sudo rabita-app` in a terminal refuses with *must not be run as
      root*.

### 4. Welcome

- [ ] Shows this computer's name, *Ubuntu 24.04…* and login screen *GNOME
      (GDM)*. **Next** is enabled.

### 5. Your organization

- [ ] **Next** is disabled until a domain is found.
- [ ] Type `LAB` and press **Find**: *Enter the full domain name with dots…*
- [ ] Type `lab.quietbyte.test` and press Enter: a panel says *Found
      lab.quietbyte.test*, how many DCs answered, which one will be used
      (and your site), and any stale-record or clock warnings in plain words.

### 6. IT admin sign-in

- [ ] **Next** stays disabled until both user name and password are filled.
- [ ] `LAB\joiner` is accepted (the `LAB\` part is dropped). **Show** /
      **Hide** reveals the password. **Advanced** has Computer name and OU.

### 7. Choose options

- [ ] *Everyone in lab.quietbyte.test* is selected; choosing *Only members
      of these groups* shows a box, and **Connect** stays disabled until a
      group is typed.
- [ ] Press **Connect**. The system's password dialog appears (polkit).
      **Wait at least 30 seconds** before typing your password, to check a
      slow answer still works.
- [ ] Pressing **Cancel** in that dialog instead keeps you on this screen
      with *Authentication was cancelled.*; **Connect** again works.

### 8. Setting up

- [ ] The checklist updates live, step by step; the mango progress bar
      fills with a moving shimmer.
- [ ] **Show details** reveals the technical log as it happens.
- [ ] ‹ Back, Next and Cancel are disabled while it runs.

### 9. Finish

- [ ] *All done* with a tick that draws itself, and three plain next steps.
      **Close** closes the window.
- [ ] Sign out; on the login screen use **Not listed?**, sign in as `amna`;
      the home folder is created.
- [ ] Reopen Rabita Setup: it opens on the **Status** screen (see the
      GUI-only checklist below).

### 10. When things go wrong

On a fresh snapshot each time:

- [ ] Wrong admin password: the Finish screen says *This computer was not
      connected* with the reason; **‹ Back** returns to IT admin sign-in with
      the password box empty.
- [ ] Joined but not finished (*lab fault injection: the terminal is only
      used to cause the fault, never to fix it*): install the packages first
      and break `pam-auth-update` as in Milestone 2 step 4b, then connect. The Finish
      screen says *Almost done*, what went wrong, and what is still to do,
      with a **Finish setup** button (no ‹ Back). Put `pam-auth-update` back
      (`sudo chmod +x /usr/sbin/pam-auth-update`), press **Finish setup**:
      only the five remaining steps run, then *All done*.
- [ ] Close Rabita Setup halfway (Cancel or Esc on screens 2–4): it asks
      *Close Rabita Setup?* first.

### 11. Remove

- [ ] See step G7 of the GUI-only checklist below.

### Known limits in this milestone

- The `.deb` is built on Ubuntu 22.04 for amd64. Other architectures and
  the `.rpm` / AUR packages come in Milestone 8.
- The admin password passes through the web page and the app's memory as
  plain text for the moment it takes to send it to the helper; the Rust
  side wipes its copy straight after. It is never written to disk.

## GUI only: install, join, sign in, leave (no terminal, no sudo)

What this proves: a technician can do everything, from installing Rabita to
removing the computer from the domain, with the mouse and the desktop's own
password window. **Do not open a terminal in this section.** If any step
seems to need one, stop: that is a bug. Take a screenshot and note it.

The only exceptions are marked *lab only*: things a technician would never
do, used to cause a fault on purpose.

### Lab for this section

- The DC with `lab.quietbyte.test`, the delegated `joiner` account, the test
  user `amna`, and a domain admin account for removing computers (or give
  `joiner` the right to delete computer objects in the Linux PCs OU).
- **VM 1, GNOME:** Ubuntu 24.04 desktop (26.04 too if you have it), signed
  in as a normal administrator account (the one created at install).
- **VM 2, KDE:** Kubuntu 24.04 desktop, same as VM 1.
- On each VM, a second **standard** (non-administrator) account, made in
  Settings → System → Users (GNOME) or System Settings → Users (KDE).
- Snapshot both VMs before starting, and restore the snapshot between the
  numbered parts when it says *fresh snapshot*.
- Have **Active Directory Users and Computers** open on the DC.

### G1. Install by double-click

Copy `rabita_0.1.0~dev_amd64.deb` (from the **rabita-deb** CI artifact) to
the VM's Downloads folder, using a shared folder or a browser download.

GNOME (VM 1):

- [ ] In **Files**, double-click the `.deb`. **App Center** opens and shows
      *Rabita Setup* with its two-nodes logo and description.
- [ ] Choose **Install**. The system password window appears; enter your
      password. App Center shows it as installed.
- [ ] Open **Show Apps** and search *Rabita* or *domain*: **Rabita Setup**
      is there with the logo. Open it.

KDE (VM 2):

- [ ] In **Dolphin**, double-click the `.deb`. **Discover** opens on
      *Rabita Setup*. Choose **Install** and enter your password.
- [ ] Open the application launcher and search *Rabita*: **Rabita Setup**
      is there under *System*. Open it.

Record for both: which app opened the `.deb`, how long the install took,
and any warning it showed (for example, about a package from outside the
store).

### G2. The admin password window (GNOME and KDE)

Do this on both VMs. Everything that changes the computer goes through the
desktop's own password window; Rabita itself never asks for the computer's
password.

- [ ] On *Your organization*, enter a domain that does not exist (for
      example `nowhere.test`) and press **Find**. The DNS box appears. Type
      `10.0.0.10` and press **Use this server**: the **desktop's** password
      window appears (GNOME: a dark sheet over the screen; KDE: a separate
      *Authentication Required* window). It names *Rabita Setup* and says it
      *needs an administrator's password to change how this computer signs
      in to your organization*.
- [ ] Press **Cancel** in that window. Rabita stays put and says nothing
      was changed (or that authentication was cancelled). Nothing changed.
- [ ] Sign out, sign in as the **standard** account, open Rabita Setup and
      repeat: the password window asks for an **administrator's** password
      (it may list the admin account to pick). Enter it: the action goes
      ahead. Sign out and back in as the admin account.
- [ ] Enter a wrong password three times: the window says it failed and
      Rabita shows a plain message; nothing was changed.
- [ ] After one successful password, other actions within a few minutes
      may not ask again. That is expected (`auth_admin_keep`).

### G3. Computer name too long (fresh snapshot)

- [ ] Rename the computer to `lab-classroom-pc-01` (19 characters): GNOME
      Settings → System → About → **Device Name**; KDE System Settings →
      About this System is read-only, so on VM 2 use the Kubuntu installer's
      name or skip this part. Restart.
- [ ] Open Rabita Setup. Welcome shows *This computer needs a new name*,
      says it is 19 characters long and the limit is 15, and offers
      `lab-classroo-01` in an editable box.
- [ ] Type `lab_pc!`: the hint turns red (*Use only letters, numbers and
      hyphens*) and **Next** is disabled. Type `-lab`, then `2024`: each has
      its own plain reason. Type `lab-pc-room12`: **Next** is enabled.
- [ ] Carry on through G5 to join. The checklist shows *Set the computer
      name* just before *Join the domain*, with *Renamed this computer
      from “lab-classroom-pc-01” to “lab-pc-room12”*.
- [ ] In Settings → About, the device name is now `lab-pc-room12`. In
      Active Directory Users and Computers, the computer object is
      **LAB-PC-ROOM12** (not a cut-off old name).

### G4. Domain not found: set the DNS server (fresh snapshot)

Make the VM use a DNS server that does not know the lab domain: in GNOME
Settings → Network → the wired connection's gear → IPv4, turn **Automatic**
off next to DNS and enter `1.1.1.1` (KDE: System Settings → Connections →
IPv4 → Other DNS servers). Apply, then turn the network off and on.

- [ ] Open Rabita Setup → **Next**. Enter `lab.quietbyte.test`, press
      **Find**: a red box says *lab.quietbyte.test was not found*, explains
      DNS in one sentence, and offers an IP box and *Look for it on this
      network*.
- [ ] Type `10.0.0` and **Use this server**: *“10.0.0” is not an IP address*
      under the box; nothing changed and no password window.
- [ ] Press **Look for it on this network**. Within about 10 seconds it
      lists the DC's IP (only if the DC is on the same network as the VM, or
      is its gateway; otherwise it says none were found, which is fine).
- [ ] Press **Use** next to the DC's IP (or type it and press **Use this
      server**). Enter your password in the desktop's window. Rabita says
      *This computer now uses 10.x.x.x as its DNS server on “Wired
      connection 1”* and immediately finds the domain. **Next** is enabled.
- [ ] Settings → Network → the gear: DNS now shows the DC's IP with
      Automatic off.
- [ ] Type a wrong IP that is not a DNS server (for example the VM's own
      gateway if it is not the DC) on a second try from a fresh snapshot:
      Rabita says the server does not know the domain and **changes
      nothing**.
- [ ] Cancel path: after Rabita changed the DNS server, press **Cancel**.
      It asks *Close Rabita Setup?* and offers **Undo and close**. Choose it:
      the window closes and Settings → Network shows the old DNS setting
      (`1.1.1.1`) again.
- [ ] Leftovers path: set the DNS server again, then **Cancel** → **Keep the
      change**. Reopen Rabita Setup: Welcome says *Settings from an earlier
      attempt* and offers **Undo changes**. Press it, enter your password:
      *Changes undone*, and the old DNS setting is back.

### G5. Join, all steps

From a fresh snapshot with working DNS, or straight after G3/G4:

- [ ] Walk through the six screens: domain → `joiner` and password →
      options (keep the defaults; type `amna` as the test user) →
      **Connect** → the desktop's password window → the live checklist.
- [ ] Every step ends with a tick, including *Set up sign-in*, *Choose who
      can sign in*, *Create home folders on first login*, *Show a username
      box on the login screen* and *Check that it works*.
- [ ] Finish: *All done*, with three plain next steps. **Close**.
- [ ] Partial join (*lab only*: break `pam-auth-update` as in Milestone 2
      step 4b to cause the fault): Finish says *Almost done* and lists what
      is still to do. Undo the fault, then press **Finish setup** in the
      window: only the remaining steps run, then *All done*. Also try
      closing instead, reopening Rabita Setup: Status says *setup is not
      finished* and offers **Finish setup** there too.

### G6. Sign in as a domain user

- [ ] Restart. The login screen has **Not listed?** (GNOME) or a username
      box (KDE/SDDM).
- [ ] Sign in as `amna` with her domain password. The desktop opens; Files
      shows a new home folder `/home/amna`.
- [ ] Sign out. Turn the VM's network off (in Proxmox, or the network icon
      on the login screen). Sign in as `amna` again: it still works
      (cached).
- [ ] Turn the network back on. Sign out, sign in as the local admin.

### G7. Status

- [ ] Open Rabita Setup. It opens on **Status**, not Welcome: *Connected to
      lab.quietbyte.test*, the computer name, the domain controller, and
      *Connected since* today's date.
- [ ] Health checks show ticks for domain controller, setup, sign-in
      service, who can sign in, computer account key, Kerberos sign-in and
      clock. A stale DNS record or bad clock shows as a mango **!** with a
      plain explanation. Hover a cut-off line to read it in full.
- [ ] Turn the network off and press **Refresh**: the domain controller and
      Kerberos checks turn red with a reason. Turn it back on and
      **Refresh**: ticks again.
- [ ] The banner shows *Status → Leave domain → Leaving → Done*.

### G8. Leave, removing the computer from the domain

- [ ] On Status press **Leave domain…**. The screen explains what leaving
      does and what is put back. *Also remove this computer from
      lab.quietbyte.test (recommended)* is ticked; **Leave domain** stays
      disabled until an admin user name and password are entered.
- [ ] Enter the admin account and press **Leave domain**: Rabita asks
      *Leave lab.quietbyte.test?* first. **Keep connected** does nothing.
      Press **Leave domain** again → **Leave** → the desktop's password
      window → the checklist: leave, restore settings, sign-in rules, login
      screen, DNS server, computer name, clean up.
- [ ] *This computer has left lab.quietbyte.test*, saying the computer
      account was removed. **Close**.
- [ ] In Active Directory Users and Computers (press F5), the computer
      object is **gone**.
- [ ] Restart. The login screen is back to normal (user list, no **Not
      listed?** if it was not there before). `amna` can no longer sign in;
      the local admin can. Her files stay in `/home/amna`.
- [ ] If G3 or G4 ran on this snapshot: Settings → About shows the
      **original** name (`lab-classroom-pc-01`) and Settings → Network shows
      the **original** DNS setting.
- [ ] Open Rabita Setup again: it opens on Welcome, ready to connect. Join
      once more to check nothing was left behind (then restore the snapshot).

### G9. Leave when the domain can't be reached

Fresh snapshot, join (G5), then make the DC unreachable: in Proxmox,
disconnect the DC's network card (or shut the DC down).

- [ ] Leave with *Also remove…* ticked and the admin account. The details
      log says *could not reach lab.quietbyte.test… Leaving on this
      computer only*.
- [ ] The result says **Left on this computer only** and explains that the
      computer account (its name) is still in the directory and the IT
      admin must delete it in **Active Directory Users and Computers**.
- [ ] Everything else is put back as in G8 (restart and check).
- [ ] Reconnect the DC. In Active Directory Users and Computers, the object
      is still there; delete it by hand as the message said.
- [ ] Also try **unticking** *Also remove…*: the admin boxes disappear, a
      note says the account stays in the directory, and **Leave domain**
      works without any admin account for the domain (the desktop's password
      window still appears).

### G10. Leave with a wrong admin password

- [ ] Fresh snapshot, join, then Leave with a wrong password for the
      domain admin account. The result says *This computer has not left
      lab.quietbyte.test* and that the username or password was not
      accepted. **‹ Back** returns to the Leave screen with the user name
      kept and the password box empty. The computer is still joined
      (Status still shows it).
- [ ] With an account that may not delete computer objects: the message
      says that account is not allowed to remove computers, and suggests a
      different account or leaving on this computer only.

### G11. Look and feel on both desktops

- [ ] Navy and mango, Manrope font, nothing cut off on any screen including
      Status (with every check), Leave, and the DNS box with results.
- [ ] Reduced motion: GNOME Settings → Accessibility → **Reduce
      Animation**; KDE System Settings → General Behaviour → Animation speed
      **Instant**. Reopen Rabita Setup: nothing moves (no pulsing, no
      rings, no slide-in, no shimmer). Note if KDE's setting does not reach
      the app.
- [ ] Every screen can be used with the keyboard alone (Tab, Space, Enter,
      Esc).

### G12. Uninstall by mouse

- [ ] GNOME: App Center → **Manage** (or search *Rabita*) → **Uninstall**
      → password. KDE: Discover → **Installed** → Rabita Setup → **Remove**.
- [ ] The menu entry disappears. Leave the domain **before** uninstalling;
      if you forget, reinstall Rabita and use Leave domain.

### Known limits of the GUI-only path

- The DNS fix needs NetworkManager (the default on Ubuntu, Kubuntu, Mint,
  Fedora and openSUSE desktops). Without it Rabita says so and changes
  nothing; the technician then sets DNS in the system's network settings.
- *Look for it on this network* only checks the gateway and the computer's
  own network (up to 254 addresses, DNS port only). A DNS server on another
  subnet must be typed in.
- The password window is the desktop's own. If none is running (for
  example over some remote-desktop tools), Rabita says no password window
  could be shown and changes nothing.
- Whether App Center or Discover accept a local `.deb` depends on their
  version. Ubuntu 24.04's App Center installs local `.deb` files through
  PackageKit; the CI and the developer sandbox check that PackageKit
  installs and removes the package without questions (`pkcon
  install-local`), but the click in App Center and Discover is only tested
  here, in the lab.

