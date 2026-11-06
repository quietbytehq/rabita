#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Installs a development build of the Rabita helper and CLI on THIS machine,
# for testing in a lab VM. It asks for sudo. Undo with scripts/dev-uninstall.sh.
#
# Never run this on a production machine: it is for the test lab only.
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v cargo >/dev/null; then
  echo "cargo is not installed. Install Rust first: https://rustup.rs" >&2
  exit 1
fi

echo "==> Building release binaries"
cargo build --release -p rabita-helper

echo "==> Installing binaries (sudo)"
sudo install -Dm755 target/release/rabita-helper /usr/local/libexec/rabita-helper
sudo install -Dm755 target/release/rabitactl /usr/local/bin/rabitactl

echo "==> Installing polkit policy, D-Bus policy and service files (sudo)"
sudo install -Dm644 packaging/polkit/org.quietbyte.rabita.policy \
  /usr/share/polkit-1/actions/org.quietbyte.rabita.policy
sudo install -Dm644 packaging/dbus/org.quietbyte.Rabita1.conf \
  /usr/share/dbus-1/system.d/org.quietbyte.Rabita1.conf
sed 's|/usr/libexec/rabita-helper|/usr/local/libexec/rabita-helper|' \
  packaging/dbus/org.quietbyte.Rabita1.service \
  | sudo install -Dm644 /dev/stdin /usr/share/dbus-1/system-services/org.quietbyte.Rabita1.service
sed 's|/usr/libexec/rabita-helper|/usr/local/libexec/rabita-helper|' \
  packaging/systemd/rabita-helper.service \
  | sudo install -Dm644 /dev/stdin /etc/systemd/system/rabita-helper.service

sudo systemctl daemon-reload
# dbus-daemon re-reads /usr/share/dbus-1/system.d on reload; dbus-broker watches it.
sudo systemctl reload dbus 2>/dev/null || true

echo
echo "Installed. Next:"
echo "  Terminal 1:  sudo /usr/local/libexec/rabita-helper      # run the helper and watch its log"
echo "  Terminal 2:  rabitactl version && rabitactl detect     # talk to it over D-Bus"
echo "(Without terminal 1, D-Bus starts the helper on demand via systemd.)"
