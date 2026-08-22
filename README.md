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

Network restrictions require nftables support on the host and the OCI hook
included in `host/`.

On Windows, Pithos runs as a native binary and talks to a Linux podman machine
(`podman machine init` + `podman machine start`). Everything the binary
executes is routed through the podman machine or through the `platform`
module, so no WSL is required on the host side.

## Windows setup

1. Install Podman Desktop or the Podman CLI, then create and start a machine:
   ```sh
   podman machine init
   podman machine start
   ```
2. Install the OCI hook and nftables inside the machine so egress caps work.
   Copy `host/oci-hooks.d/pithos-egress-cap.json` and
   `host/hooks/pithos-egress-cap.sh` into the machine under
   `$HOME/.config/containers/oci/hooks.d/`, `chmod +x` the script, and install
   nftables with the machine's package manager (`sudo apt install nftables`).
3. Build and run:
   ```sh
   pithos build
   pithos run
   ```

The hook script reads the ruleset from the `pithos.networking-rules`
annotation, so it never depends on a host path that a Windows host cannot
provide. `pithos run` verifies the setup with `podman machine ssh` and prints
the hook path found inside the machine.

## Usage

Create a starter configuration:

```sh
pithos init
pithos init --toolchain rust
pithos init --toolchain python
```

`init` creates `./pithos.toml` and does not overwrite an existing file.

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
configuration. It works as a global option or on the `build` and `run`
subcommands:

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

The id is optional while exactly one session is running.

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
image_tag = "localhost/pithos-opencode:latest"
workspace = "/workspace"

[harness]
name = "opencode"
command = ["opencode", "/workspace"]
```

### Container settings

`base_image` is the base container image. It defaults to
`node:22-bookworm-slim`.

`image_tag` is the local Podman image tag. It defaults to
`localhost/pithos-opencode:latest`.

`workspace` is the absolute path used as the working directory inside the
container. It defaults to `/workspace`.

`install` is a list of Debian packages installed with `apt-get`. It defaults to
`["git", "gcc", "libc6-dev"]`.

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
(`aqua:LuaLS/lua-language-server`, `npm:pyright`, `cargo:just`). Tools are
registered with `mise use -g --yes` and their shims are placed on the
container's `PATH`, so they resolve both during the image build and inside
sessions. Prefer this key for tools covered by the mise registry and use
`downloads` for raw installer scripts that mise cannot express. Legacy
plugin-based backends need `git`, which this key does not install implicitly.

```toml
mise = ["neovim", "lua-language-server", "node@22"]
```

### Harness settings

The required `[harness]` table selects the agent harness. The currently
supported harness is OpenCode:

```toml
[harness]
name = "opencode"
command = ["opencode", "/workspace"]
```

`command` is the command run when the container starts. It defaults to
`["opencode", "/workspace"]`.

`[harness.allowlist]` configures OpenCode permissions. Values can be
`allow`, `ask`, or `deny`, depending on the operation.

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

Harness-specific persistent data is mounted into the container according to the
selected harness. For OpenCode, its session database directory is mounted
read-write so sessions created on the host or in the container are available to
both. OpenCode credentials can be enabled with `credentials = true` inside the
`[harness]` table; the authentication file remains read-only.

`[environment]` contains environment variables passed to the container:

```toml
[environment]
TERM = "xterm-256color"
```

### Networking

The optional `[networking]` table restricts container egress through nftables.
At least one of `payload_size` or `quota` must be set, and values are in KiB.

`payload_size` limits the original payload size of each connection.

`quota` sets the total egress budget for the session.

`whitelist` adds HTTPS hosts that bypass these limits.

`use_default_whitelist` controls the default hosts. It is `true` by default
and includes `opencode.ai`, `mcp.exa.ai`, and `api.exa.ai`.

```toml
[networking]
payload_size = 8
quota = 102400
whitelist = ["proxy.example.com"]
use_default_whitelist = true
```

## Development

Run formatting and tests with:

```sh
cargo fmt --check
cargo test
```
