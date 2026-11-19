#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Removes what scripts/dev-install.sh installed (this version or an older
# one). Files that belong to an installed rabita package are left alone.
# Does NOT leave the domain: leave first (Rabita Setup → Leave domain, or
# `rabitactl leave`) if the machine is joined.
set -euo pipefail

DEV_UNIT=/etc/systemd/system/rabita-helper-dev.service
OLD_OVERRIDE=/etc/systemd/system/rabita-helper.service
ACTIVATION=/usr/share/dbus-1/system-services/org.quietbyte.Rabita1.service
POLICY=/usr/share/polkit-1/actions/org.quietbyte.rabita.policy
BUS_POLICY=/usr/share/dbus-1/system.d/org.quietbyte.Rabita1.conf

# True if an installed package owns the file.
packaged() {
  dpkg -S "$1" >/dev/null 2>&1 || rpm -qf "$1" >/dev/null 2>&1
}

# True if the file was written by dev-install.sh (now or in the past).
ours() {
  [ -f "$1" ] && grep -q -e "scripts/dev-install.sh" -e "/usr/local/libexec/rabita-helper" "$1"
}

sudo systemctl stop rabita-helper-dev.service 2>/dev/null || true
remove=(/usr/local/libexec/rabita-helper /usr/local/bin/rabitactl "$DEV_UNIT")
# Older dev-install.sh wrote this override of the packaged unit.
if ours "$OLD_OVERRIDE"; then
  sudo systemctl stop rabita-helper.service 2>/dev/null || true
  remove+=("$OLD_OVERRIDE")
fi
if ours "$ACTIVATION" && ! packaged "$ACTIVATION"; then
  remove+=("$ACTIVATION")
fi
for f in "$POLICY" "$BUS_POLICY"; do
  if [ -e "$f" ] && ! packaged "$f"; then
    remove+=("$f")
  fi
done
sudo rm -f "${remove[@]}"
sudo systemctl daemon-reload 2>/dev/null || true
sudo systemctl reload dbus 2>/dev/null || true
echo "Removed the development build. Rabita's state and backups in /var/lib/rabita were left in place."
