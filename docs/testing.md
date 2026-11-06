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

- [ ] `found: true`, at least one controller with `reachable: true` and a
      `clock_offset_secs` close to 0 (anything under 120 is fine).
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
      is filled in; `joined_at` is now.
- [ ] `sudo cat /etc/sssd/sssd.conf` contains
      `use_fully_qualified_names = False`, `fallback_homedir = /home/%u`,
      `cache_credentials = True`, `default_domain_suffix = lab.quietbyte.test`.
- [ ] `sudo ls /var/lib/rabita/backups/*/` lists the backed-up files
      (`etc__krb5.conf`, `etc__pam.d__common-session`, …).
- [ ] On the DC: the computer object exists in the Linux PCs OU.

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

- SDDM (KDE) gets no login-screen change yet; most SDDM themes already offer
  "Other".
- Who may sign in is now a choice (all domain users, or only listed groups).
  Separately, `ad_gpo_access_control` is set to `permissive`, so Windows
  "Allow log on locally" GPOs are not enforced on Linux machines; the group
  list above is the way to restrict access.
- Fedora, openSUSE and Arch backends are placeholders (Milestone 7).

## Milestone 3 — wizard GUI

_To be written with the milestone._
