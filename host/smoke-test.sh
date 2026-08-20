#!/bin/sh
# Smoke test for the pithos egress-cap OCI hook.
# Loads a policy-drop ruleset into the container netns via a private hook
# directory, then verifies egress is actually blocked.
set -eu

image="${1:-localhost/pithos-opencode:latest}"
runtime_dir="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/pithos"
mkdir -p "$runtime_dir"
test_dir=$(mktemp -d "$runtime_dir/smoke.XXXXXX")
hooks="$test_dir/hooks"
mkdir -p "$hooks"
trap 'rm -rf "$test_dir"' EXIT HUP INT TERM

hook=$(CDPATH= cd -- "$(dirname -- "$0")/hooks" && pwd)/pithos-egress-cap.sh
json="$hooks/pithos-egress-cap.json"

if [ ! -x "$hook" ]; then
  echo "hook is missing or not executable: $hook" >&2
  exit 1
fi

cat > "$json" <<EOF
{
  "version": "1.0.0",
  "hook": {
    "path": "$hook",
    "args": ["pithos-egress-cap.sh"]
  },
  "when": {
    "annotations": {
      "pithos.networking": "1"
    }
  },
  "stages": ["createRuntime"]
}
EOF

# Inline rules in the same form the pithos binary writes: single line,
# no quotes, statements separated with "; " and every rule "; "-terminated.
inline_rules='table inet pithos-egress { chain output { type filter hook output priority filter; policy drop; oifname lo accept; } }'

echo "starting container; egress should be blocked by the hook..."
podman --hooks-dir "$hooks" run --rm \
  --annotation "pithos.networking=1" \
  --annotation "pithos.networking-rules=$inline_rules" \
  "$image" sh -c \
  'if curl -fsS --max-time 10 https://opencode.ai >/dev/null 2>&1; then
     echo "NOT ENFORCED: egress was not blocked" >&2
     exit 1
   fi
   echo "ENFORCED: egress was blocked"'
