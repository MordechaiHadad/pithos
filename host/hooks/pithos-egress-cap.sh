#!/bin/sh
set -eu

state=$(cat)

pid=$(printf '%s' "$state" | sed -n 's/.*"pid":[[:space:]]*\([0-9][0-9]*\).*/\1/p')
rules=$(printf '%s' "$state" | sed -n 's/.*"pithos.networking-rules":"\([^"]*\)".*/\1/p')

if [ -z "$rules" ]; then
  rules="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/pithos/networking.nft"
fi

if [ -z "$pid" ] || [ ! -f "$rules" ]; then
  exit 1
fi

podman unshare nsenter -t "$pid" -n /usr/sbin/nft delete table inet pithos-egress 2>/dev/null || true
podman unshare nsenter -t "$pid" -n /usr/sbin/nft -f "$rules"
