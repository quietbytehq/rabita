#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Installs a development build of the Rabita helper and CLI on THIS machine,
# for testing in a lab VM. It asks for sudo. Undo with scripts/dev-uninstall.sh.
#
# Never run this on a production machine: it is for the test lab only.
#
# It must never get in the way of the real package, so it:
#   - refuses to run while the rabita package is installed;
#   - uses its own systemd unit, rabita-helper-dev.service, and never writes
#     /etc/systemd/system/rabita-helper.service (that name belongs to the
#     package; a copy in /etc would silently replace the packaged unit);
#   - writes the D-Bus activation file where the package's own goes, so
#     installing the package later simply replaces it;
#   - marks every file it writes with DEV_MARKER, so the package and
#     dev-uninstall.sh can tell its files from anything else.
set -euo pipefail
cd "$(dirname "$0")/.."

DEV_MARKER="Installed by rabita scripts/dev-install.sh (development build, not the package)"
DEV_UNIT=/etc/systemd/system/rabita-helper-dev.service
OLD_OVERRIDE=/etc/systemd/system/rabita-helper.service
ACTIVATION=/usr/share/dbus-1/system-services/org.quietbyte.Rabita1.service

if dpkg-query -W -f='${Status}' rabita 2>/dev/null | grep -q "install ok installed" \
  || rpm -q rabita >/dev/null 2>&1; then
  cat >&2 <<'EOF'
The Rabita package is installed on this computer. A development build would
replace parts of it. Remove the package first (App Center, or
`sudo apt remove rabita`), or test the package instead.
EOF
  exit 1
fi

if ! command -v cargo >/dev/null; then
  echo "cargo is not installed. Install Rust first: https://rustup.rs" >&2
  exit 1
fi

echo "==> Building release binaries"
cargo build --release -p rabita-helper

echo "==> Installing binaries (sudo)"
sudo install -Dm755 target/release/rabita-helper /usr/local/libexec/rabita-helper
sudo install -Dm755 target/release/rabitactl /usr/local/bin/rabitactl

echo "==> Installing polkit policy and D-Bus policy (sudo)"
sudo install -Dm644 packaging/polkit/org.quietbyte.rabita.policy \
  /usr/share/polkit-1/actions/org.quietbyte.rabita.policy
sudo install -Dm644 packaging/dbus/org.quietbyte.Rabita1.conf \
  /usr/share/dbus-1/system.d/org.quietbyte.Rabita1.conf

echo "==> Installing $DEV_UNIT and the D-Bus activation file (sudo)"
{
  echo "# $DEV_MARKER"
  sed -e 's|/usr/libexec/rabita-helper|/usr/local/libexec/rabita-helper|' \
    -e 's|^Description=\(.*\)$|Description=\1 [development build]|' \
    packaging/systemd/rabita-helper.service
} | sudo install -Dm644 /dev/stdin "$DEV_UNIT"
{
  echo "# $DEV_MARKER"
  sed -e 's|/usr/libexec/rabita-helper|/usr/local/libexec/rabita-helper|' \
    -e 's|^SystemdService=.*$|SystemdService=rabita-helper-dev.service|' \
    packaging/dbus/org.quietbyte.Rabita1.service
} | sudo install -Dm644 /dev/stdin "$ACTIVATION"

# Earlier versions of this script wrote $OLD_OVERRIDE, which overrides the
# packaged unit. Remove it if it is ours (it starts /usr/local/libexec).
if [ -f "$OLD_OVERRIDE" ] && grep -q "/usr/local/libexec/rabita-helper" "$OLD_OVERRIDE"; then
  echo "==> Removing $OLD_OVERRIDE left by an older dev-install.sh (sudo)"
  sudo systemctl stop rabita-helper.service 2>/dev/null || true
  sudo rm -f "$OLD_OVERRIDE"
fi

sudo systemctl daemon-reload
# dbus-daemon re-reads its configuration and activation files on reload;
# dbus-broker watches them.
sudo systemctl reload dbus 2>/dev/null || true

echo
echo "Installed. Next:"
echo "  Terminal 1:  sudo /usr/local/libexec/rabita-helper      # run the helper and watch its log"
echo "  Terminal 2:  rabitactl version && rabitactl detect     # talk to it over D-Bus"
echo "(Without terminal 1, D-Bus starts the helper on demand via rabita-helper-dev.service.)"
echo "Remove it with scripts/dev-uninstall.sh before installing the rabita package."
