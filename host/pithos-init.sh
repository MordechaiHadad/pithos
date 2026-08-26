#!/bin/sh
# Container entrypoint: load session egress rules into this network
# namespace, then drop to the unprivileged agent identity and exec the
# harness command supplied as arguments.
set -eu

rules="${PITHOS_EGRESS_RULES:-}"
if [ -n "$rules" ]; then
  nft="$(command -v nft || true)"
  [ -n "$nft" ] || nft=/usr/sbin/nft
  if [ ! -x "$nft" ]; then
    echo "pithos-init: nft is missing or not executable at $nft" >&2
    exit 1
  fi
  rules_file="${TMPDIR:-/tmp}/pithos-egress.$$"
  trap 'rm -f "$rules_file"' EXIT HUP INT TERM
  printf '%s\n' "$rules" | tr ';' '\n' > "$rules_file"
  "$nft" -f "$rules_file"
fi

: "${PITHOS_AGENT_UID:?pithos-init requires PITHOS_AGENT_UID}"
: "${PITHOS_AGENT_GID:?pithos-init requires PITHOS_AGENT_GID}"
exec setpriv --reuid="$PITHOS_AGENT_UID" --regid="$PITHOS_AGENT_GID" \
  --clear-groups --inh-caps=-all --ambient-caps=-all --bounding-set=-all "$@"
