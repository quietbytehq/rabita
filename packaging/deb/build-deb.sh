#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Builds target/deb/rabita_<version>_<arch>.deb: the Rabita Setup wizard,
# the root helper, rabitactl, and the polkit, D-Bus and systemd files they
# need. Double-clicking the .deb installs it with the desktop's software app
# (App Center, GNOME Software, GDebi); `sudo apt install ./rabita_*.deb`
# works everywhere.
#
# Usage: packaging/deb/build-deb.sh [--no-build]
#   --no-build  package the binaries already in target/release
# Needs: cargo, npm, dpkg-deb and dpkg-shlibdeps (package dpkg-dev).
#
# Build on the oldest release you want to support: a package built on
# Ubuntu 22.04 also installs on 24.04, 26.04, Debian 12+ and Linux Mint 21+.
set -euo pipefail
umask 022
cd "$(dirname "$0")/../.."
root=$PWD

build=1
[ "${1:-}" = "--no-build" ] && build=0

version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
debversion=${version//-/\~} # 0.1.0-dev -> 0.1.0~dev (sorts before 0.1.0)
arch=$(dpkg --print-architecture)

if [ "$build" = 1 ]; then
  npm --prefix app/ui ci --no-audit --no-fund
  npm --prefix app/ui run build
  cargo build --release --locked -p rabita-app -p rabita-helper
fi

out="$root/target/deb"
stage="$out/rabita_${debversion}_${arch}"
rm -rf "$stage"
mkdir -p "$stage/DEBIAN"

put() { # mode source destination
  install -D -m "$1" "$2" "$stage$3"
}
put 755 target/release/rabita-app /usr/bin/rabita-app
put 755 target/release/rabitactl /usr/bin/rabitactl
put 755 target/release/rabita-helper /usr/libexec/rabita-helper
put 644 packaging/polkit/org.quietbyte.rabita.policy /usr/share/polkit-1/actions/org.quietbyte.rabita.policy
put 644 packaging/dbus/org.quietbyte.Rabita1.conf /usr/share/dbus-1/system.d/org.quietbyte.Rabita1.conf
put 644 packaging/dbus/org.quietbyte.Rabita1.service /usr/share/dbus-1/system-services/org.quietbyte.Rabita1.service
put 644 packaging/systemd/rabita-helper.service /usr/lib/systemd/system/rabita-helper.service
put 644 packaging/desktop/org.quietbyte.Rabita.desktop /usr/share/applications/org.quietbyte.Rabita.desktop
put 644 packaging/metainfo/org.quietbyte.Rabita.metainfo.xml /usr/share/metainfo/org.quietbyte.Rabita.metainfo.xml
put 644 packaging/icons/org.quietbyte.Rabita.svg /usr/share/icons/hicolor/scalable/apps/org.quietbyte.Rabita.svg
for size in 32 48 64 128 256 512; do
  put 644 "packaging/icons/png/$size.png" "/usr/share/icons/hicolor/${size}x${size}/apps/org.quietbyte.Rabita.png"
done

# Copyright: our licence, plus the full text of the font's OFL (not in
# /usr/share/common-licenses).
doc="$stage/usr/share/doc/rabita"
mkdir -p "$doc"
{
  cat packaging/deb/copyright.in
  echo
  echo "License: OFL-1.1"
  sed -e 's/^$/./' -e 's/^/ /' app/ui/public/fonts/OFL.txt
} >"$doc/copyright"
chmod 644 "$doc/copyright"
printf 'rabita (%s) unstable; urgency=medium\n\n  * Build from %s.\n\n -- QuietByte <quietbytehq@users.noreply.github.com>  %s\n' \
  "$debversion" "$(git rev-parse --short HEAD 2>/dev/null || echo source)" "$(date -R)" |
  gzip -9n >"$doc/changelog.gz"
chmod 644 "$doc/changelog.gz"

for script in postinst prerm postrm; do
  install -m 755 "packaging/deb/$script" "$stage/DEBIAN/$script"
done

# Work out the shared libraries the binaries need (WebKitGTK, GTK, ...).
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/debian"
printf 'Source: rabita\n\nPackage: rabita\nArchitecture: any\n' >"$tmp/debian/control"
shlibs=$(cd "$tmp" && dpkg-shlibdeps -O \
  -e"$stage/usr/bin/rabita-app" -e"$stage/usr/bin/rabitactl" -e"$stage/usr/libexec/rabita-helper" |
  sed -n 's/^shlibs:Depends=//p')

installed_size=$(du -sk --exclude=DEBIAN "$stage" | cut -f1)
cat >"$stage/DEBIAN/control" <<CONTROL
Package: rabita
Version: $debversion
Architecture: $arch
Maintainer: QuietByte <quietbytehq@users.noreply.github.com>
Installed-Size: $installed_size
Depends: $shlibs, dbus, polkitd | policykit-1
Section: admin
Priority: optional
Homepage: https://github.com/quietbytehq/rabita
Description: connect this computer to your organization's domain
 Rabita Setup is a friendly wizard that joins a Linux desktop to an
 Active Directory domain, so staff can sign in at the normal login screen
 with their work username and password, even when the network is down.
 .
 It installs and configures the standard tools (realmd, SSSD, adcli) for
 you, checks every step, and backs up each file it changes so the
 connection can be undone.
CONTROL

deb="$out/rabita_${debversion}_${arch}.deb"
dpkg-deb --root-owner-group -Zxz --build "$stage" "$deb" >/dev/null
echo "$deb"
