#!/bin/sh
set -eu

state=$(cat)

pid=$(printf '%s' "$state" | sed -n 's/.*"pid"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p')
rules=$(printf '%s' "$state" | sed -n 's/.*"pithos.networking-rules"[[:space:]]*:[[:space:]]*"\([^"\\]*\)".*/\1/p')

if [ -z "$pid" ]; then
  echo "pithos egress hook: OCI state has no container pid" >&2
  exit 1
fi

if [ -z "$rules" ]; then
  rules="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/pithos/networking.nft"
fi

if [ ! -r "$rules" ]; then
  echo "pithos egress hook: rules file is missing or unreadable: $rules" >&2
  exit 1
fi

nft=$(command -v nft 2>/dev/null || true)
[ -n "$nft" ] || nft=/usr/sbin/nft
if [ -z "$nft" ] || [ ! -x "$nft" ]; then
  echo "pithos egress hook: nft is not executable" >&2
  exit 1
fi

podman unshare nsenter -t "$pid" -n "$nft" delete table inet pithos-egress 2>/dev/null || :
podman unshare nsenter -t "$pid" -n "$nft" -f "$rules"
