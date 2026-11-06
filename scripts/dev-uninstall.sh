#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Removes what scripts/dev-install.sh installed. Does NOT leave the domain:
# run `rabitactl leave` first if the machine is joined.
set -euo pipefail
sudo systemctl stop rabita-helper.service 2>/dev/null || true
sudo rm -f \
  /usr/local/libexec/rabita-helper \
  /usr/local/bin/rabitactl \
  /usr/share/polkit-1/actions/org.quietbyte.rabita.policy \
  /usr/share/dbus-1/system.d/org.quietbyte.Rabita1.conf \
  /usr/share/dbus-1/system-services/org.quietbyte.Rabita1.service \
  /etc/systemd/system/rabita-helper.service
sudo systemctl daemon-reload
sudo systemctl reload dbus 2>/dev/null || true
echo "Removed. Rabita's state and backups in /var/lib/rabita were left in place."
