#!/bin/sh
# Verifies egress enforcement for a running pithos session from the host.
#
# Usage: host/verify-egress.sh [container]
# Without an argument the newest running container is checked.
set -eu

if [ "$#" -gt 0 ]; then
  container="$1"
else
  container=$(podman ps -q | head -n1)
fi
[ -n "$container" ] || { echo "FAIL: no running container"; exit 1; }

pid=$(podman inspect -f '{{.State.Pid}}' "$container" 2>/dev/null || echo 0)
[ -n "$pid" ] && [ "$pid" != "0" ] || { echo "FAIL: $container is not running"; exit 1; }

annotation=$(podman inspect \
  -f '{{index .Config.Annotations "pithos.networking"}}' "$container" 2>/dev/null || echo "")
[ "$annotation" = "1" ] || {
  echo "FAIL: pithos.networking annotation missing; the hook will never fire"
  exit 1
}

rules=$(podman unshare nsenter -t "$pid" -n nft list table inet pithos-egress 2>&1)
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

echo "OK: egress table loaded in $container (pid $pid)"
echo "behavioral check (run inside the session):"
echo "  pithos exec -- curl -m3 -o /dev/null http://<your-lan-ip>/"
echo "  must time out or be refused; an HTTP response means rules are bypassed"
