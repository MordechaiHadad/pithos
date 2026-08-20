#!/bin/sh
set -eu

state=$(cat)

pid=$(printf '%s' "$state" | sed -n 's/.*"pid"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p')
rules=$(printf '%s' "$state" | sed -n 's/.*"pithos.networking-rules"[[:space:]]*:[[:space:]]*"\([^"\\]*\)".*/\1/p')

if [ -z "$pid" ]; then
  echo "pithos egress hook: OCI state has no container pid" >&2
  exit 1
fi

nft=$(command -v nft 2>/dev/null || true)
[ -n "$nft" ] || nft=/usr/sbin/nft
if [ -z "$nft" ] || [ ! -x "$nft" ]; then
  echo "pithos egress hook: nft is not executable" >&2
  exit 1
fi

# Remove any ruleset left over from a previous run in this netns.
podman unshare nsenter -t "$pid" -n "$nft" delete table inet pithos-egress 2>/dev/null || :

if [ -z "$rules" ]; then
  echo "pithos egress hook: annotation pithos.networking-rules is missing or unreadable" >&2
  exit 1
fi

# The rules arrive inline in the annotation so the hook does not depend on a
# host path that may be unreachable (e.g. a Windows path on a podman machine).
# Materialize them inside the filesystem the hook runs on, splitting on ';'
# into the multi-line form nft accepts as input.
rules_file=$(mktemp "${TMPDIR:-${XDG_RUNTIME_DIR:-/tmp}}/pithos-egress.XXXXXX") ||
  { echo "pithos egress hook: cannot create rules file" >&2; exit 1; }
trap 'rm -f "$rules_file"' EXIT HUP INT TERM
printf '%s\n' "$rules" | tr ';' '\n' > "$rules_file"

podman unshare nsenter -t "$pid" -n "$nft" -f "$rules_file"