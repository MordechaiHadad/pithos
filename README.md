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

An alternate configuration can be selected with the global `--config` option:

```sh
pithos --config ./other.toml run
```

Without `--config`, Pithos looks for `./pithos.toml` and then the global
configuration at `$XDG_CONFIG_HOME/pithos/pithos.toml`.

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

`install` is a list of Debian packages installed with `apt-get`.

`toolchains` is a list of supported toolchains to install. Supported values
are `rust` and `python`.

```toml
```

The Rust toolchain installs Rust through rustup and rust-analyzer. The Python
toolchain installs uv, ruff, and pyright.

`cargo` is a list of Rust crates installed with `cargo install`. Specifying
Cargo packages also installs Rust even when `rust` is not listed in
`toolchains`.

`npm` is a list of packages installed globally with npm.

`bun` is a list of packages installed globally with Bun. Bun itself is
installed when this list is not empty.

`uv` is a list of uv tools. Each entry has a required `name` and optional
`python` and `run` values.

```toml
uv = [
    { name = "serena-agent", python = "3.13", run = "serena init" },
]
```

Specifying uv tools also installs uv even when `python` is not listed in
`toolchains`.

`downloads` is a list of shell installer scripts to run during the image
build. Each entry has a required `url` and optional environment variables.

```toml
    { url = "https://deno.land/install.sh", env = { DENO_INSTALL = "/usr/local" } },
]
```

Only use download URLs that you trust.

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

```toml
```

`[credentials]` controls credentials mounted into the container. Currently,
`opencode = true` mounts the local OpenCode authentication file when it exists.

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
