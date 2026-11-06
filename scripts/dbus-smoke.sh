#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Starts a private session D-Bus, runs rabita-helper on it, and checks that
# rabitactl can call the unprivileged methods and that a privileged method is
# refused when polkit cannot be reached. No root, no network, no system change.
# Used by CI and handy locally: scripts/dbus-smoke.sh [path/to/target/debug]
set -euo pipefail
BIN="${1:-$(dirname "$0")/../target/debug}"
TMP="$(mktemp -d)"
ADDR="unix:path=$TMP/bus.sock"
cleanup() {
  [ -n "${HPID:-}" ] && kill "$HPID" 2>/dev/null || true
  [ -n "${DPID:-}" ] && kill "$DPID" 2>/dev/null || true
  rm -rf "$TMP"
}
trap cleanup EXIT

dbus-daemon --session --address="$ADDR" --nofork --nopidfile >/dev/null 2>&1 &
DPID=$!
sleep 0.5
export DBUS_SESSION_BUS_ADDRESS="$ADDR"
"$BIN/rabita-helper" --session 2>"$TMP/helper.log" &
HPID=$!
for _ in $(seq 1 20); do
  grep -q "serving org.quietbyte.Rabita1" "$TMP/helper.log" 2>/dev/null && break
  sleep 0.25
done
grep -q "serving org.quietbyte.Rabita1" "$TMP/helper.log" || { echo "helper did not start:"; cat "$TMP/helper.log"; exit 1; }

fail() { echo "FAIL: $*" >&2; exit 1; }

out="$("$BIN/rabitactl" --session version)"
[[ "$out" == rabita-helper* ]] || fail "version: $out"

out="$("$BIN/rabitactl" --session detect)"
grep -q '"distro_id"' <<<"$out" || fail "detect: $out"

out="$("$BIN/rabitactl" --session status)"
grep -q '"joined": false' <<<"$out" || fail "status: $out"

out="$("$BIN/rabitactl" --session discover LAB)"
grep -q 'full domain name' <<<"$out" || fail "discover short name: $out"

# Privileged call must be refused (no polkit on a private bus), and must not
# start a job.
set +e
out="$(echo pw | "$BIN/rabitactl" --session join lab.example.test --user joiner --password-stdin 2>&1)"
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "join was not refused"
grep -Eq 'AuthFailed|AccessDenied' <<<"$out" || fail "join refusal: $out"

busy="$(busctl --address="$ADDR" get-property org.quietbyte.Rabita1 /org/quietbyte/Rabita1 org.quietbyte.Rabita1 Busy)"
[ "$busy" = "b false" ] || fail "helper busy after refused join: $busy"

echo "D-Bus smoke test OK"
