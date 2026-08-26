#!/bin/sh
# Verifies egress enforcement for a running pithos session from the host.
#
# Usage: scripts/verify-egress.sh [container]
# Without an argument the newest running container is checked.
set -eu

if [ "$#" -gt 0 ]; then
  container="$1"
else
  container=$(podman ps -q | head -n1)
fi
[ -n "$container" ] || { echo "FAIL: no running container"; exit 1; }

rules=$(podman exec "$container" nft list table inet pithos-egress 2>&1) || {
  echo "FAIL: no pithos-egress table loaded in $container"
  exit 1
}
for range in 10.0.0.0/8 172.16.0.0/12 192.168.0.0/16 169.254.0.0/16; do
  printf '%s\n' "$rules" | grep -qF "$range" || {
    echo "FAIL: $range is not dropped; table missing or outdated"
    exit 1
  }
done
printf '%s\n' "$rules" | grep -qF 'drop' || {
  echo "FAIL: table exists but has no drop statements"
  exit 1
}

echo "OK: egress rules enforced in $container"
echo "behavioral checks (run inside the session):"
echo "  pithos exec -- curl -m3 -o /dev/null http://<your-lan-ip>/   must fail"
echo "  pithos exec -- curl -m3 -o /dev/null https://opencode.ai     must work within quota"
