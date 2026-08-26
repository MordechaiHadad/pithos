#!/bin/sh
# Smoke test for the pithos egress entrypoint.
# Feeds a policy-drop ruleset through the same environment variable the
# pithos binary uses, then verifies egress is actually blocked.
set -eu

image="${1:-localhost/pithos-opencode:latest}"
uid="$(id -u)"
gid="$(id -g)"

# Inline rules in the same form the pithos binary writes: single line,
# no quotes, statements separated with ";" and every rule ";"-terminated.
inline_rules='table inet pithos-egress { chain output { type filter hook output priority filter; policy drop; oifname lo accept; }; }'

echo "starting container; egress should be blocked by the entrypoint..."
podman run --rm \
  --userns=keep-id \
  --read-only \
  --tmpfs "/tmp:rw,mode=1777" \
  --cap-drop=ALL --cap-add=NET_ADMIN --cap-add=SETUID --cap-add=SETGID --cap-add=SETPCAP \
  --security-opt=no-new-privileges \
  --env "PITHOS_EGRESS_RULES=$inline_rules" \
  --env "PITHOS_AGENT_UID=$uid" \
  --env "PITHOS_AGENT_GID=$gid" \
  "$image" sh -c \
  'if curl -fsS --max-time 10 https://opencode.ai >/dev/null 2>&1; then
     echo "NOT ENFORCED: egress was not blocked" >&2
     exit 1
   fi
   echo "ENFORCED: egress was blocked"'
