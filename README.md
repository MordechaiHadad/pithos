# Pithos

Pithos runs disposable agent workspaces in Podman containers. It copies the
current Git repository into a temporary workspace, runs the configured harness,
and can apply the changes back to the repository after the session finishes.

The container image is built from `pithos.toml`. The default harness is
OpenCode, but the configuration format keeps the harness settings separate
from the container settings.

## Requirements

- Rust and Cargo, to build Pithos
- Podman
- A Git repository to run a session in

Workspace copies use filesystem copy-on-write clones where available (btrfs,
XFS, ZFS, APFS, ReFS); other filesystems silently fall back to regular copies.

Network restrictions work out of the box; no host-side setup or privileges
are required.

On macOS and Windows, Pithos runs as a native binary and talks to a Linux
podman machine (`podman machine init` + `podman machine start`). Everything the
binary executes is routed through the podman machine or through the `platform`
module, so no Linux compatibility layer is required on the host side.

## macOS and Windows setup

1. Install Podman Desktop or the Podman CLI, then create and start a machine:
   ```sh
   podman machine init
   podman machine start
   ```
2. Build and run:
   ```sh
   pithos build
   pithos run
   ```

Egress rules ship inside the image itself, so nothing has to be installed in
the podman machine.

## Usage

Create a starter configuration:

```sh
pithos init
```

`init` creates `./pithos.toml` and does not overwrite an existing file.
Toolchains are user-defined: uncomment and fill in a `[toolchain.NAME]`
table (or add one to the global library) to install tools.

Build the configured container image:

```sh
pithos build
```

Run a session in the current Git repository:

```sh
pithos run
```

Running `pithos` without a subcommand is equivalent to `pithos run`.

Use `--yes` to apply changes without asking for confirmation, or `--no` to
discard changes without asking. These options cannot be used together.

A toolchain can be selected on the command line instead of listing it in the
configuration. The flag is accepted by a bare invocation (which implies
`run`) and by the `build` and `run` subcommands:

```sh
pithos --toolchain rust
pithos run --toolchain rust
pithos build --toolchain python
```

`--toolchain` (or `-t`) accepts any toolchain name that resolves to a
definition (global library or project configuration) and overrides
the `toolchains` key in the configuration. It uses an image tag suffixed with
the toolchain name, so `pithos run --toolchain rust` reuses an existing
`...:latest-rust` image when it already matches the configuration.

An alternate configuration can be selected with the global `--config` option:

```sh
pithos --config ./other.toml run
```

Without `--config`, Pithos looks for `./pithos.toml` and then the global
configuration at `$XDG_CONFIG_HOME/pithos/pithos.toml`.

## Inspecting a live session

While `pithos run` is active, the sandbox stays reachable from other
terminals. Pithos records each running session under
`$XDG_RUNTIME_DIR/pithos` (or the system temporary directory when unset):

- `pithos ps` lists running sessions with their ids and sandbox paths.
- `pithos shell [id]` opens an interactive shell inside the running container.
- `pithos exec [id] -- <command>` runs a single command inside the container.
- `pithos path [id]` prints the host path of the live workspace.
- `pithos pull [id]` applies the live workspace back to the host repository
  without ending the session.

The id is optional while exactly one session is running.

### Pulling changes while a session runs

`pithos pull` reuses the end-of-session review while the harness keeps
running: it reports added, modified, and deleted files, shows a diff on
request (`[v]iew`), and mirrors the workspace into the repository after
confirmation. The sandbox is never modified, so pulls can repeat as the agent
keeps working, and the normal review still runs when the session finishes.

By default the pull targets the repository the session started from.
`--path` redirects it to any existing directory; relative forms such as `.`
or `..` resolve against your shell's working directory:

```sh
pithos pull myrepo-1a2b --path ../second-checkout
```

Pass `--dry-run` to preview without applying anything. Pass `--json` to emit
a machine-readable report for editor plugins:

```sh
pithos pull myrepo-1a2b --dry-run --json
pithos pull myrepo-1a2b --yes --json
```

The JSON object contains `session`, `target`, `applied`, and `changed`, where
each entry has a `path` and a `kind` of `added`, `modified`, or `deleted`.
When stdin is not a terminal, `pull` refuses to prompt and requires an
explicit `--yes`, `--no`, or `--dry-run`. The `--yes` and `--no` flags are
global, so they work before or after the subcommand.

The container's workspace is a bind mount of a host directory, so any editor
can open the path printed by `pithos path` and watch the agent's changes land
in real time without ending the session. Edits saved there are included in
the review step when the session finishes. Editor plugins can discover
sessions by watching the registry directory and shell out to `pithos exec`
for terminal integration.

## Configuration

The configuration file is TOML. A minimal configuration is:

```toml
base_image = "node:22-bookworm-slim"
workspace = "/workspace"

[harness]
name = "opencode"
command = ["opencode", "/workspace"]
```

### Container settings

`base_image` is the base container image. It defaults to
`node:22-bookworm-slim`. Any image works as long as it provides `node`, `npm`,
and `useradd`; pithos provisions its own unprivileged agent user (uid/gid taken
from the invoking host user, home `/home/agent`) instead of relying on the base
image's default user. After building, `host/smoke-test-agent.sh <image>`
verifies the session environment: agent-home ownership, writable state/cache
mounts, and that every installed tool resolves for the runtime user.

`image_tag` is the local Podman image tag. It defaults to
`localhost/pithos-<harness-name>:latest` (for example
`localhost/pithos-claude-code:latest` for the claude-code harness), so each
harness builds into its own image namespace. Set it explicitly to override.

`workspace` is the absolute path used as the working directory inside the
container. It defaults to `/workspace`.

The root filesystem is mounted read-only, and `/usr`, `/etc`, and friends are
additionally stripped of directory write bits at build time. Baked toolchains
live under `/usr/local/share/mise` and are used in place — nothing is copied
when a session starts. The agent home is an ephemeral tmpfs: fully writable,
starts clean every session, and everything in it (including runtime-installed
tools) is discarded when the container exits. Tool state that should survive —
harness databases, credentials, history — is mounted explicitly; see the
harness sections below.

Sessions refuse to start when invoked as the root host user: a non-root agent
combined with dropped capabilities is what keeps system paths untouchable.

`install` is a list of Debian packages installed with `apt-get`. It defaults to
`["git", "gcc", "libc6-dev", "ncurses-term"]`. The terminfo database shipped by
`ncurses-term` lets tools resolve the host terminal's `TERM` value inside the
container.

`toolchains` is a list of toolchain names to install. Every toolchain is
user-defined: a name must resolve to a `[toolchain.NAME]` table in the project
configuration or in the global toolchain library described below. For example,
a Rust toolchain provisioned through mise looks like:

```toml
toolchains = ["rust"]

[toolchain.rust]
install = ["gcc", "libc6-dev"]
mise = ["rust"]
extra = ["mise use -g --yes 'rust-analyzer'"]
```

```toml
toolchains = ["golang"]

[toolchain.golang]
install = ["ca-certificates", "curl"]
env = { PATH = "/usr/local/go/bin:$PATH" }
run = [
    "curl -fsSL https://go.dev/dl/go1.24.0.linux-amd64.tar.gz | tar -C /usr/local -xz",
]
```

A `[toolchain.NAME]` table accepts the same installation keys as the global
lists below (`install`, `cargo`, `npm`, `bun`, `uv`, `downloads`, `mise`)
scoped to that toolchain, plus:

`includes` is a list of other toolchain names installed whenever this one is.
Expansion is transitive, and included toolchains behave as if they were listed
in `toolchains` themselves, including their `extra` commands. The include
graph must not contain cycles.

```toml
[toolchain.fullstack]
includes = ["rust", "web"]

[toolchain.web]
npm = ["typescript", "eslint"]
```

`install` is a list of Debian packages installed with `apt-get` before the
toolchain itself.

`env` is a table of environment variables set before the toolchain installs.

`run` is a list of shell commands that install the toolchain. They run whenever
the toolchain is installed, including when it is pulled in implicitly by
another toolchain's package lists.

`extra` is a list of shell commands that run only when the toolchain is
explicitly listed in `toolchains` or selected with `--toolchain`.

### Global toolchain library

Toolchains shared across projects live in
`$XDG_CONFIG_HOME/pithos/toolchains.toml`. The file contains only
`[toolchain.NAME]` tables using the same schema. Project definitions take
precedence over the library.

`cargo` is a list of Rust crates installed with `cargo install`. Specifying
Cargo packages also installs Rust even when `rust` is not listed in
`toolchains`.

`npm` is a list of packages installed globally with npm. A non-empty list also
installs Node through mise.

`bun` is a list of packages installed globally with Bun. Bun itself is
installed through mise when this list is not empty.

`uv` is a list of uv tools. Each entry has a required `name` and optional
`python` and `run` values.

```toml
uv = [
    { name = "serena-agent", python = "3.13", run = "serena init" },
]
```

Specifying uv tools also installs uv even when `python` is not listed in
`toolchains`. Their executables are installed in `/usr/local/bin` so commands
from `run` values are available during the build and in sessions.

`downloads` is a list of shell installer scripts to run during the image
build. Each entry has a required `url` and optional environment variables.

```toml
downloads = [
    { url = "https://deno.land/install.sh", env = { DENO_INSTALL = "/usr/local" } },
]
```

Only use download URLs that you trust.

`mise` is a list of tools installed through [mise](https://mise.jdx.dev). Each
entry uses the spec forms mise accepts: a registry shorthand with an optional
version (`neovim`, `node@22`) or a full backend name
(`aqua:LuaLS/lua-language-server`, `npm:pyright`, `cargo:just`). Shims are
placed on the container's `PATH`, so tools resolve during the image build and
inside sessions. Prefer this key for tools covered by the mise registry and
use `downloads` for raw installer scripts that mise cannot express. Legacy
plugin-based backends need `git`, which this key does not install implicitly.

```toml
mise = ["neovim", "lua-language-server", "node@22"]
```

### Harness settings

The required `[harness]` table selects the agent harness. Two harnesses are
currently supported, OpenCode and Claude Code:

```toml
[harness]
name = "opencode"
command = ["opencode", "/workspace"]
```

```toml
[harness]
name = "claude-code"
credentials = true
```

`command` is the command run when the container starts. It defaults to
`["opencode", "/workspace"]` for OpenCode and `["claude"]` for Claude Code.
Claude Code treats positional arguments as prompts, so the default command is
deliberately bare; the container already starts in the workspace directory.

Harness definitions are stored in `harnesses/*.toml` and embedded into the
binary during the Cargo build. The definition contains the install command,
default command, and repeated `[[mount]]` entries. Each mount has a closed
`type` (`credentials`, `state`, `config`, `ephemeral`, or `generated`) and an
independent `access` (`ro`, `pinned`, `pinned_dir`, or `tmpfs`), plus a
`host_base` such as `home`, `data:claude-code`, or `state:opencode`.

Users can add or override harnesses without rebuilding Pithos by placing TOML
files in `$XDG_CONFIG_HOME/pithos/harnesses/` (normally
`~/.config/pithos/harnesses/`). User definitions take precedence over the
embedded definitions with the same name. Invalid user files are ignored with
a warning. A minimal custom harness is:

```toml
schema_version = 1
name = "my-agent"
install = "RUN npm install --global my-agent\n"
command = ["my-agent"]

[[mount]]
host = ".my-agent"
target = "/home/agent/.my-agent"
type = "config"
access = "ro"
host_base = "home"
```

#### OpenCode paths

OpenCode state lives under the host's XDG directories. Sessions mount
`auth.json` read-only when `credentials = true`, pin the session database
(`opencode.db`, `-wal`, `-shm`) and small state files (`kv.json`,
`session.json`, `model.json`, `prompt-history.jsonl`) read-write to their
host locations so history survives across sessions, mount
`~/.config/opencode` read-only when credentials or an allowlist are set, and
cover everything else with tmpfs.

#### Claude Code paths

Claude Code sessions mirror the same persistence model:

| Container path | Host backing | Mode |
| --- | --- | --- |
| `~/.claude/.credentials.json` | `~/.claude/.credentials.json` | read-only, gated on `credentials` |
| `~/.claude.json` | `~/.local/share/claude-code/claude.json` | read-write |
| `~/.claude/projects` | `~/.local/share/claude-code/projects` | read-write directory |
| `~/.claude/history.jsonl` | `~/.local/share/claude-code/history.jsonl` | read-write |
| `~/.claude/settings.json` | generated per-session file | read-only |
| `~/.claude/{CLAUDE.md,keybindings.json,skills,agents,commands,rules,output-styles,themes,workflows}` | same host paths | read-only when they exist |

Transcripts, auto memory, and prompt history therefore survive across
sessions, while `todos`, `shell-snapshots`, and `statsig` stay ephemeral.

On macOS, Claude Code keeps OAuth credentials in the system Keychain instead
of `.credentials.json`. Pithos cannot mount Keychain items; when
`credentials = true` and no credentials file exists on a macOS host, it warns
before starting an unauthenticated session. Either export the credentials
once:

```sh
security find-generic-password -s "Claude Code-credentials" -w \
  > ~/.claude/.credentials.json && chmod 600 ~/.claude/.credentials.json
```

or generate a long-lived token with `claude setup-token` and pass it through
the global configuration:

```toml
[environment]
CLAUDE_CODE_OAUTH_TOKEN = "..."
```

Read-only credential mounts mean OAuth token rotation cannot be written
back; re-run `/login` on the host if a session reports expired tokens.

#### Allowlists

`[harness.allowlist]` configures permissions. For OpenCode the object is
passed through verbatim as `OPENCODE_CONFIG_CONTENT`. For Claude Code the
supported keys are translated into a generated `settings.json` whose
`permissions` key replaces the user's own (all other keys, such as `model`
or `hooks`, are preserved):

| pithos | Claude Code rule | verdict list |
| --- | --- | --- |
| `bash."git *" = "allow"` | `"Bash(git *)"` | `permissions.allow` |
| `bash."*" = "ask"` | `"Bash(*)"` | `permissions.ask` |
| `bash."curl -T *" = "deny"` | `"Bash(curl -T *)"` | `permissions.deny` |
| `edit = "allow"` | `"Edit"`, `"Write"` | that verdict's list |

Claude Code evaluates deny, then ask, then allow. Other allowlist keys are
rejected for Claude Code at configuration time; verify the effective rules
with `claude /permissions`.

```toml
[harness.allowlist]
edit = "allow"

[harness.allowlist.bash]
"*" = "ask"
"git *" = "allow"
```

### Repository and environment settings

`exclusions` is a list of paths excluded when comparing or applying changes.

`diff_viewer` is an optional command used to review changes. It must contain a
`{dir}` placeholder, which is replaced with the temporary workspace path.

`copy_strategy` forces the tier used to populate the session workspace:
`reflink` (kernel-level copy-on-write clones), `worktree` (a shared-object git
clone whose origin `.git/objects` is mounted read-only into the container), or
`copy` (a plain tree copy). When unset or `"auto"`, pithos probes for reflink
support first, falls back to the worktree tier on filesystems without
copy-on-write support (for example ext4), and keeps the plain copy as a last
resort.

Setting `credentials = true` inside `[harness]` mounts harness authentication
files read-only into the container.

`[environment]` contains environment variables passed to the container:

```toml
[environment]
EDITOR = "nvim"
```

Terminal-related variables (`TERM`, `COLORTERM`, `TERM_PROGRAM`,
`TERM_PROGRAM_VERSION`, and `NO_COLOR`) are forwarded automatically from the
host shell to `run`, `shell`, and `exec`, so harnesses like OpenCode detect the
same color support inside the sandbox as outside it and truecolor themes render
identically. Entries in `[environment]` take precedence over forwarded values.

### Networking

Egress is restricted by default: every session runs under nftables rules
without any configuration. Values are in KiB.

`payload_size` limits the original payload size of each connection
(default 65536, 64 MiB).

`quota` sets the total egress budget for the session
(default 2097152, 2 GiB).

`block_private` drops traffic to RFC1918/link-local destinations so sessions
cannot reach the host LAN, router, or NAS. DNS (port 53) remains allowed.
It is `true` by default.

`whitelist` adds HTTPS hosts that bypass these limits.

`use_default_whitelist` controls the built-in fast-lane hosts. It is `true`
by default and includes `opencode.ai`, `api.anthropic.com` (Claude Code),
the Exa search APIs (`mcp.exa.ai`,
`api.exa.ai`), the Parallel Web Systems search endpoints (`api.parallel.ai`,
`search.parallel.ai`, `task-mcp.parallel.ai`), and common agent search
providers (`api.tavily.com`, `api.search.brave.com`, `google.serper.dev`).

```toml
[networking]
enabled = false # only way to turn restrictions off
payload_size = 65536
quota = 2097152
whitelist = ["proxy.example.com"]
use_default_whitelist = true
block_private = true
```

Rules load automatically before your harness starts; sessions refuse to boot
if enforcement cannot be established.

### Audio

`audio = true` forwards the host's PulseAudio-compatible socket into the
session so harnesses such as OpenCode can play attention sounds. It requires
a Linux host with a sound server exposing `$XDG_RUNTIME_DIR/pulse/native`
(PipeWire provides this through `pipewire-pulse`, which is the default on
modern distributions); other platforms currently run without sound.

```toml
audio = true
```

The image gains the `libasound2` and `libpulse0` client libraries when this
is enabled, and `[environment]` can still override `XDG_RUNTIME_DIR` or
`PULSE_SERVER`. Note that sharing a sound server socket grants the sandbox
access to host audio output and potentially microphone input.

## Development

Run formatting and tests with:

```sh
just fmt-check
just test
```

Common tasks live in a [justfile](https://github.com/casey/just); `just` alone
lists them, including `just run` for a session with phase timings and
`just flamegraph` for CPU profiling. Profiling needs
[cargo-flamegraph](https://github.com/flamegraph-rs/flamegraph) plus `perf`
(relax with `sudo sysctl kernel.perf_event_paranoid=1`) or `dtrace` on macOS;
set `RAYON_NUM_THREADS=1` for cleaner stacks.
