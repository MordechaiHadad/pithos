use eyre::{Result, WrapErr, bail};
use std::collections::BTreeSet;
use std::fs;
use std::process::Command;

use crate::agent::{AGENT_HOME, AGENT_USER};
use crate::config::{Config, Download, ToolchainDef, UvTool};
use crate::sandbox::TempDir;

impl Config {
    #[tracing::instrument(skip(self), fields(image_tag = %self.image_tag))]
    pub(crate) fn build_image(&self) -> Result<()> {
        let context = TempDir::create("pithos-build")?;
        let config_digest = self.digest()?;
        tracing::debug!(config_digest, "building image");
        fs::write(context.0.join("Containerfile"), self.containerfile()?)
            .wrap_err("cannot write Containerfile")?;
        let status = Command::new("podman")
            .args([
                "build",
                "--tag",
                &self.image_tag,
                "--label",
                &format!("pithos.config={config_digest}"),
                "--file",
                "Containerfile",
            ])
            .arg(&context.0)
            .status()
            .wrap_err("could not execute podman build")?;
        tracing::debug!(%status, "podman build finished");
        if !status.success() {
            bail!("podman build failed")
        }
        Ok(())
    }

    pub(crate) fn image_up_to_date(&self) -> Result<bool> {
        let output = Command::new("podman")
            .args([
                "image",
                "inspect",
                "--format",
                "{{ index .Labels \"pithos.config\" }}",
                &self.image_tag,
            ])
            .output()
            .wrap_err("could not inspect podman image")?;
        if !output.status.success() {
            tracing::trace!(image_tag = %self.image_tag, "image not present yet");
            return Ok(false);
        }
        let stored = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let local_digest = self.digest()?;
        let up_to_date = stored == local_digest;
        tracing::trace!(stored_digest = %stored, %local_digest, up_to_date, "image digest comparison");
        Ok(up_to_date)
    }

    fn digest(&self) -> Result<String> {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::hash::DefaultHasher::new();
        self.containerfile()?.hash(&mut hasher);
        Ok(format!("{:016x}", hasher.finish()))
    }

    fn containerfile(&self) -> Result<String> {
        let mut output = format!("FROM {}\nWORKDIR {}\n", self.base_image, self.workspace);
        output.push_str(&agent_preamble());
        output.push_str(&self.harness.install());
        if let Some(line) = install_line(
            "apt-get update && apt-get install -y --no-install-recommends",
            " && rm -rf /var/lib/apt/lists/*",
            &self.install,
        ) {
            output.push_str(&line);
        }
        let defs = self.merged_toolchains();
        let selected: BTreeSet<String> = self.expanded_selection(&defs)?.into_iter().collect();
        let wanted = self.expanded_selection(&defs)?;
        let mut providers: Vec<String> = Vec::new();
        if !self.cargo.is_empty() {
            providers.push("rust".into());
        }
        if !self.npm.is_empty() {
            providers.push("node".into());
        }
        if !self.bun.is_empty() {
            providers.push("bun".into());
        }
        if !self.uv.is_empty() {
            providers.push("python".into());
            providers.push("uv".into());
        }
        let mut covered: BTreeSet<&String> = self.mise.iter().collect();
        covered.extend(wanted.iter().flat_map(|name| defs[name].mise.iter()));
        providers.retain(|provider| !covered.contains(provider));
        let mut mise_tools = self.mise.clone();
        mise_tools.extend(providers);
        if !mise_tools.is_empty() || wanted.iter().any(|name| !defs[name].mise.is_empty()) {
            output.push_str(
                "ENV MISE_DATA_DIR=/usr/local/share/mise PATH=/usr/local/share/mise/shims:$PATH\n",
            );
        }
        if !self.uv.is_empty() || wanted.iter().any(|name| !defs[name].uv.is_empty()) {
            output.push_str("ENV UV_TOOL_BIN_DIR=/usr/local/bin\n");
        }
        for name in &wanted {
            let def = &defs[name];
            output.push_str(&def.containerfile_block(selected.contains(name)));
        }
        if let Some(line) = mise_install_lines(&mise_tools) {
            output.push_str(&line);
        }
        if let Some(line) = install_line("cargo install --root /usr/local", "", &self.cargo) {
            output.push_str(&line);
        }
        if let Some(line) = install_line("npm install --global", "", &self.npm) {
            output.push_str(&line);
        }
        if let Some(line) = install_line("bun install --global", "", &self.bun) {
            output.push_str(&line);
        }
        for tool in &self.uv {
            output.push_str(&uv_install_lines(tool));
        }
        for download in &self.downloads {
            output.push_str(&format!("RUN {}\n", download.command()));
        }
        output.push_str(&agent_epilogue());
        output.push_str("RUN chmod -R a+rwX /tmp\n");
        output.push_str("CMD ");
        output.push_str(&json_command(self.harness.command()));
        output.push('\n');
        Ok(output)
    }
}

impl ToolchainDef {
    fn containerfile_block(&self, selected: bool) -> String {
        let mut block = String::new();
        if !self.env.is_empty() {
            let pairs = self
                .env
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join(" ");
            block.push_str(&format!("ENV {pairs}\n"));
        }
        if let Some(line) = install_line(
            "apt-get update && apt-get install -y --no-install-recommends",
            " && rm -rf /var/lib/apt/lists/*",
            &self.install,
        ) {
            block.push_str(&line);
        }
        for command in &self.run {
            block.push_str(&format!("RUN {command}\n"));
        }
        if let Some(line) = mise_install_lines(&self.mise) {
            block.push_str(&line);
        }
        if selected {
            for command in &self.extra {
                block.push_str(&format!("RUN {command}\n"));
            }
        }
        if let Some(line) = install_line("cargo install --root /usr/local", "", &self.cargo) {
            block.push_str(&line);
        }
        if let Some(line) = install_line("npm install --global", "", &self.npm) {
            block.push_str(&line);
        }
        if let Some(line) = install_line(
            "npm install --global bun && bun install --global",
            "",
            &self.bun,
        ) {
            block.push_str(&line);
        }
        for tool in &self.uv {
            block.push_str(&uv_install_lines(tool));
        }
        for download in &self.downloads {
            block.push_str(&format!("RUN {}\n", download.command()));
        }
        block
    }
}

fn agent_preamble() -> String {
    let uid = crate::platform::current_uid();
    let gid = crate::platform::current_gid();
    let mut preamble = format!(
        "RUN mkdir -p {} && (useradd -o -m -d {} -u {uid} -g {gid} -s /bin/sh {} || true)\n",
        shell_quote(AGENT_HOME),
        shell_quote(AGENT_HOME),
        shell_quote(AGENT_USER),
    );
    preamble.push_str(&format!("ENV HOME={AGENT_HOME}\n"));
    preamble
}

fn agent_epilogue() -> String {
    let uid = crate::platform::current_uid();
    let gid = crate::platform::current_gid();
    format!("RUN chown -R {uid}:{gid} {}\n", shell_quote(AGENT_HOME))
}

fn uv_install_lines(tool: &UvTool) -> String {
    let name = shell_quote(&tool.name);
    let mut lines = match &tool.python {
        Some(python) => format!(
            "RUN uv tool install {name} --python {}\n",
            shell_quote(python)
        ),
        None => format!("RUN uv tool install {name}\n"),
    };
    if let Some(run) = &tool.run {
        lines.push_str(&format!("RUN {run}\n"));
    }
    lines
}

fn mise_install_lines(items: &[String]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let quoted = items
        .iter()
        .map(|item| shell_quote(item))
        .collect::<Vec<_>>()
        .join(" ");
    let bootstrap = "(command -v mise >/dev/null 2>&1 || (apt-get update && apt-get install -y --no-install-recommends curl ca-certificates && rm -rf /var/lib/apt/lists/* && curl -fsSL https://mise.run | MISE_INSTALL_PATH=/usr/local/bin/mise sh))";
    Some(format!("RUN {bootstrap} && mise use -g --yes {quoted}\n"))
}

impl Download {
    fn command(&self) -> String {
        let env = self
            .env
            .iter()
            .map(|(key, value)| format!("{key}={}", shell_quote(value)))
            .collect::<Vec<_>>()
            .join(" ");
        let prefix = "apt-get update && apt-get install -y --no-install-recommends curl ca-certificates unzip && rm -rf /var/lib/apt/lists/*";
        let env_prefix = if env.is_empty() {
            String::new()
        } else {
            format!("{env} ")
        };
        format!(
            "{prefix} && curl -fsSL {} | {env_prefix}sh",
            shell_quote(&self.url)
        )
    }
}

fn install_line(prefix: &str, suffix: &str, items: &[String]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let quoted = items
        .iter()
        .map(|item| shell_quote(item))
        .collect::<Vec<_>>()
        .join(" ");
    Some(format!("RUN {prefix} {quoted}{suffix}\n"))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn json_command(command: &[String]) -> String {
    format!(
        "[{}]",
        command
            .iter()
            .map(|item| format!("\"{}\"", item.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Config {
        fn rendered(&self) -> String {
            self.containerfile().unwrap()
        }

        fn with_rust_toolchain() -> Self {
            toml::from_str(
                r#"
                toolchains = ["rust"]

                [harness]
                name = "opencode"

                [toolchain.rust]
                install = ["gcc", "libc6-dev"]
                mise = ["rust"]
                extra = ["mise use -g --yes 'rust-analyzer'"]
                "#,
            )
            .unwrap()
        }

        fn with_python_toolchain() -> Self {
            toml::from_str(
                r#"
                toolchains = ["python"]

                [harness]
                name = "opencode"

                [toolchain.python]
                mise = ["python", "uv"]
                extra = ["mise use -g --yes 'ruff'", "npm install --global pyright"]
                "#,
            )
            .unwrap()
        }

        fn with_cargo() -> Self {
            toml::from_str(
                r#"
                cargo = ["just"]

                [harness]
                name = "opencode"
                "#,
            )
            .unwrap()
        }

        fn with_uv() -> Self {
            toml::from_str(
                r#"
                [harness]
                name = "opencode"

                [[uv]]
                name = "serena-agent"
                python = "3.13"
                run = "serena init"

                [[uv]]
                name = "plain-tool"
                "#,
            )
            .unwrap()
        }
    }

    #[test]
    fn cargo_block_bootstraps_rust_via_mise() {
        let file = Config::with_cargo().rendered();
        assert!(file.contains("RUN cargo install --root /usr/local 'just'\n"));
        let mise = file.find("(command -v mise").unwrap();
        let rust = file.find("mise use -g --yes 'rust'\n").unwrap();
        let install = file
            .find("RUN cargo install --root /usr/local 'just'\n")
            .unwrap();
        assert!(mise < rust && rust < install);
        assert!(file.contains(
            "ENV MISE_DATA_DIR=/usr/local/share/mise PATH=/usr/local/share/mise/shims:$PATH\n"
        ));
        assert!(file.contains("apt-get install -y --no-install-recommends 'git' 'gcc'"));
    }

    #[test]
    fn build_steps_write_to_the_runtime_home() {
        let file = Config::with_cargo().rendered();
        let uid = crate::platform::current_uid();
        let gid = crate::platform::current_gid();
        assert!(
            file.contains(&format!(
                "RUN mkdir -p '/home/agent' && (useradd -o -m -d '/home/agent' -u {uid} -g {gid} -s /bin/sh 'agent' || true)\n"
            )),
            "preamble missing: {file}"
        );
        assert!(file.contains("ENV HOME=/home/agent\n"));
        assert!(file.contains("RUN cargo install --root /usr/local 'just'\n"));
        assert!(file.contains(&format!("RUN chown -R {uid}:{gid} '/home/agent'\n")));
        let chown = file
            .find(&format!("RUN chown -R {uid}:{gid} '/home/agent'\n"))
            .unwrap();
        assert!(chown < file.find("CMD ").unwrap());
    }

    #[test]
    fn package_lists_infer_providers_before_use() {
        let config: Config = toml::from_str(
            r#"
            mise = ["deno"]
            cargo = ["just"]
            bun = ["htmx"]
            uv = [{ name = "serena-agent", python = "3.13", run = "serena init" }]

            [harness]
            name = "opencode"
            "#,
        )
        .unwrap();
        let file = config.rendered();
        assert!(file.contains("mise use -g --yes 'deno' 'rust' 'bun' 'python' 'uv'\n"));
        assert!(file.contains("ENV UV_TOOL_BIN_DIR=/usr/local/bin\n"));
        let mise = file.find("mise use -g --yes 'deno'").unwrap();
        let uv_env = file.find("ENV UV_TOOL_BIN_DIR=/usr/local/bin\n").unwrap();
        let cargo = file
            .find("RUN cargo install --root /usr/local 'just'\n")
            .unwrap();
        let bun = file.find("RUN bun install --global 'htmx'\n").unwrap();
        let uv_install = file.find("RUN uv tool install 'serena-agent'").unwrap();
        assert!(mise < cargo);
        assert!(mise < bun);
        assert!(uv_env < uv_install && uv_install < file.find("RUN serena init\n").unwrap());
    }

    #[test]
    fn user_defined_rust_block_precedes_global_cargo_install() {
        let config: Config = toml::from_str(
            r#"
            toolchains = ["rust"]
            cargo = ["just"]

            [harness]
            name = "opencode"

            [toolchain.rust]
            install = ["gcc", "libc6-dev"]
            mise = ["rust"]
            "#,
        )
        .unwrap();
        let file = config.rendered();
        let rust = file.find("mise use -g --yes 'rust'\n").unwrap();
        let install = file
            .find("RUN cargo install --root /usr/local 'just'\n")
            .unwrap();
        assert!(rust < install);
    }

    #[test]
    fn mise_block_bootstraps_mise_then_installs_tools() {
        let config: Config = toml::from_str(
            r#"
            mise = ["neovim", "lua-language-server"]

            [harness]
            name = "opencode"
            "#,
        )
        .unwrap();
        let file = config.rendered();
        assert!(file.contains(
            "ENV MISE_DATA_DIR=/usr/local/share/mise PATH=/usr/local/share/mise/shims:$PATH"
        ));
        assert!(file.contains("(command -v mise >/dev/null 2>&1 || (apt-get update && apt-get install -y --no-install-recommends curl ca-certificates && rm -rf /var/lib/apt/lists/* && curl -fsSL https://mise.run | MISE_INSTALL_PATH=/usr/local/bin/mise sh)) && mise use -g --yes 'neovim' 'lua-language-server'\n"));
    }

    #[test]
    fn mise_block_absent_when_no_tools() {
        let config: Config = toml::from_str("[harness]\nname = \"opencode\"").unwrap();
        let file = config.rendered();
        assert!(!file.contains("mise"));
    }

    #[test]
    fn scoped_mise_list_renders_before_extra() {
        let config: Config = toml::from_str(
            r#"
            toolchains = ["lua"]

            [harness]
            name = "opencode"

            [toolchain.lua]
            mise = ["stylua"]
            extra = ["stylua --version"]
            "#,
        )
        .unwrap();
        let file = config.rendered();
        assert!(file.contains("mise use -g --yes 'stylua'\nRUN stylua --version\n"));
    }

    #[test]
    fn bare_mise_toolchain_installs_runtime_only() {
        let config: Config = toml::from_str(
            r#"
            toolchains = ["runtime"]

            [harness]
            name = "opencode"

            [toolchain.runtime]
            install = ["curl", "ca-certificates", "git"]
            env = { MISE_DATA_DIR = "/usr/local/share/mise", PATH = "/usr/local/share/mise/shims:$PATH" }
            run = ["curl -fsSL https://mise.run | MISE_INSTALL_PATH=/usr/local/bin/mise sh"]
            "#,
        )
        .unwrap();
        let file = config.rendered();
        assert!(file.contains(
            "RUN curl -fsSL https://mise.run | MISE_INSTALL_PATH=/usr/local/bin/mise sh\n"
        ));
        assert!(file.contains(
            "ENV MISE_DATA_DIR=/usr/local/share/mise PATH=/usr/local/share/mise/shims:$PATH"
        ));
        assert!(!file.contains("mise use -g"));
    }

    #[test]
    fn every_line_is_a_valid_instruction() {
        let config: Config = toml::from_str(
            r#"
            toolchains = ["custom"]
            install = ["git"]
            cargo = ["just"]
            npm = ["prettier"]
            bun = ["htmx"]
            uv = [{ name = "serena-agent", python = "3.13", run = "serena init" }]
            mise = ["neovim"]

            [harness]
            name = "opencode"

            [[downloads]]
            url = "https://example.com/install.sh"

            [toolchain.custom]
            install = ["curl"]
            env = { FOO = "bar" }
            run = ["make bootstrap"]
            extra = ["custom --version"]
            cargo = ["fd-find"]
            npm = ["tsx"]
            bun = ["zip"]
            uv = [{ name = "ruff" }]
            mise = ["stylua"]
            downloads = [{ url = "https://example.com/custom.sh" }]
            "#,
        )
        .unwrap();
        for line in config.rendered().lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let instruction = line.split_whitespace().next().unwrap();
            assert!(
                matches!(
                    instruction,
                    "FROM"
                        | "WORKDIR"
                        | "RUN"
                        | "ENV"
                        | "CMD"
                        | "ARG"
                        | "LABEL"
                        | "COPY"
                        | "ADD"
                        | "EXPOSE"
                        | "VOLUME"
                        | "USER"
                        | "ENTRYPOINT"
                        | "SHELL"
                        | "STOPSIGNAL"
                        | "HEALTHCHECK"
                        | "ONBUILD"
                        | "COMMENT"
                ),
                "unexpected instruction {instruction:?} in line {line:?}"
            );
        }
    }

    #[test]
    fn cargo_block_absent_when_no_crates() {
        let config: Config = toml::from_str("[harness]\nname = \"opencode\"").unwrap();
        let file = config.rendered();
        assert!(!file.contains("cargo install"));
    }

    #[test]
    fn toolchain_flag_selects_user_defined_toolchain_and_retags() {
        let mut config: Config = toml::from_str(
            r#"
            [harness]
            name = "opencode"

            [toolchain.lua]
            mise = ["lua"]
            "#,
        )
        .unwrap();
        config.with_toolchain(Some("lua".into()));
        let file = config.rendered();
        assert!(file.contains("mise use -g --yes 'lua'\n"));
        assert_eq!(config.image_tag, "localhost/pithos-opencode:latest-lua");
    }

    #[test]
    fn rust_toolchain_installs_mise_rust_analyzer_and_build_deps() {
        let file = Config::with_rust_toolchain().rendered();
        assert!(file.contains("mise use -g --yes 'rust'\n"));
        assert!(file.contains("apt-get install -y --no-install-recommends 'gcc' 'libc6-dev'"));
        assert!(file.contains("RUN mise use -g --yes 'rust-analyzer'\n"));
        assert!(!file.contains("cargo install"));
    }

    #[test]
    fn python_toolchain_installs_python_uv_ruff_and_pyright() {
        let file = Config::with_python_toolchain().rendered();
        assert!(file.contains("mise use -g --yes 'python' 'uv'\n"));
        assert!(file.contains("RUN mise use -g --yes 'ruff'\n"));
        assert!(file.contains("RUN npm install --global pyright\n"));
    }

    #[test]
    fn uv_without_toolchain_skips_ruff_and_pyright() {
        let file = Config::with_uv().rendered();
        assert!(file.contains("RUN uv tool install 'serena-agent' --python '3.13'\n"));
        assert!(!file.contains("ruff"));
        assert!(!file.contains("pyright"));
    }

    #[test]
    fn uv_block_installs_uv_and_tools() {
        let config: Config = toml::from_str(
            r#"
            toolchains = ["python"]

            [[uv]]
            name = "serena-agent"
            python = "3.13"
            run = "serena init"

            [[uv]]
            name = "plain-tool"

            [harness]
            name = "opencode"

            [toolchain.python]
            mise = ["python", "uv"]
            "#,
        )
        .unwrap();
        let file = config.rendered();
        assert!(file.contains("mise use -g --yes 'python' 'uv'\n"));
        assert!(file.contains(
            "ENV MISE_DATA_DIR=/usr/local/share/mise PATH=/usr/local/share/mise/shims:$PATH"
        ));
        assert!(
            file.contains("RUN uv tool install 'serena-agent' --python '3.13'\nRUN serena init")
        );
        assert!(file.contains("RUN uv tool install 'plain-tool'\n"));
    }

    #[test]
    fn custom_definition_expands_env_install_run_extra() {
        let config: Config = toml::from_str(
            r#"
            toolchains = ["golang"]

            [harness]
            name = "opencode"

            [toolchain.golang]
            install = ["ca-certificates"]
            env = { PATH = "/usr/local/go/bin:$PATH" }
            run = ["curl -fsSL https://go.dev/dl/go.tar.gz | tar -C /usr/local -xz"]
            extra = ["go version"]
            "#,
        )
        .unwrap();
        let file = config.rendered();
        assert!(file.contains("ENV PATH=/usr/local/go/bin:$PATH\n"));
        assert!(file.contains("apt-get install -y --no-install-recommends 'ca-certificates' && rm -rf /var/lib/apt/lists/*\n"));
        assert!(file.contains(
            "RUN curl -fsSL https://go.dev/dl/go.tar.gz | tar -C /usr/local -xz\nRUN go version\n"
        ));
    }

    #[test]
    fn extra_steps_only_when_explicitly_selected() {
        let config: Config = toml::from_str(
            r#"
            toolchains = ["golang"]

            [harness]
            name = "opencode"

            [toolchain.golang]
            run = ["install-go"]
            extra = ["go version"]
            "#,
        )
        .unwrap();
        let file = config.rendered();
        assert!(file.contains("RUN install-go\nRUN go version\n"));
    }

    #[test]
    fn extra_steps_skipped_for_undefined_selection() {
        let config: Config = toml::from_str(
            r#"
            toolchains = ["golang"]

            [harness]
            name = "opencode"

            [toolchain.golang]
            run = ["install-go"]
            extra = ["go version"]

            [toolchain.other]
            run = ["install-other"]
            extra = ["other --version"]
            "#,
        )
        .unwrap();
        let file = config.rendered();
        assert!(file.contains("RUN install-go\nRUN go version\n"));
        assert!(!file.contains("install-other"));
        assert!(!file.contains("other --version"));
    }

    #[test]
    fn scoped_package_lists_expand_inside_their_block() {
        let config: Config = toml::from_str(
            r#"
            toolchains = ["web"]

            [harness]
            name = "opencode"

            [toolchain.web]
            npm = ["typescript"]
            bun = ["htmx"]
            uv = [{ name = "ruff", python = "3.13" }]
            downloads = [{ url = "https://example.com/install.sh", env = { FOO = "bar" } }]
            "#,
        )
        .unwrap();
        let file = config.rendered();
        assert!(file.contains("RUN npm install --global 'typescript'\n"));
        assert!(file.contains("bun install --global 'htmx'\n"));
        assert!(file.contains("RUN uv tool install 'ruff' --python '3.13'\n"));
        assert!(file.contains("curl -fsSL 'https://example.com/install.sh' | FOO='bar' sh"));
        assert!(!file.contains("pyright"));
    }

    #[test]
    fn includes_install_members_and_run_their_extras() {
        let config: Config = toml::from_str(
            r#"
            toolchains = ["fullstack"]

            [harness]
            name = "opencode"

            [toolchain.fullstack]
            includes = ["rust", "web"]

            [toolchain.rust]
            mise = ["rust"]
            extra = ["mise use -g --yes 'rust-analyzer'"]

            [toolchain.web]
            run = ["npm install --global typescript"]
            extra = ["tsc --version"]
            "#,
        )
        .unwrap();
        let file = config.rendered();
        assert!(file.contains("mise use -g --yes 'rust'\n"));
        assert!(file.contains("RUN mise use -g --yes 'rust-analyzer'\n"));
        assert!(file.contains("RUN npm install --global typescript\n"));
        assert!(file.contains("RUN tsc --version\n"));
    }

    #[test]
    fn digest_changes_with_custom_definition_edit() {
        let base = r#"
            toolchains = ["golang"]

            [harness]
            name = "opencode"

            [toolchain.golang]
            run = ["install-go"]
        "#;
        let first: Config = toml::from_str(base).unwrap();
        let second: Config = toml::from_str(&base.replace("install-go", "install-go-2")).unwrap();
        assert_ne!(first.digest().unwrap(), second.digest().unwrap());
    }

    #[test]
    fn uv_block_absent_when_no_tools() {
        let config: Config = toml::from_str("[harness]\nname = \"opencode\"").unwrap();
        let file = config.rendered();
        assert!(!file.contains("uv"));
    }

    #[test]
    fn containerfile_opens_tmp_to_all_users() {
        let config: Config = toml::from_str("[harness]\nname = \"opencode\"").unwrap();
        let file = config.rendered();
        let chmod = file.find("RUN chmod -R a+rwX /tmp\n").unwrap();
        assert!(chmod < file.find("CMD ").unwrap());
    }

    #[test]
    fn config_digest_changes_with_tools() {
        let mut config = Config::with_uv();
        config.uv[0].python = Some("3.12".into());
        assert_ne!(
            config.digest().unwrap(),
            Config::with_uv().digest().unwrap()
        );
    }

    #[test]
    fn download_generates_curl_piped_run() {
        let config: Config = toml::from_str(
            r#"
            [harness]
            name = "opencode"

            [[downloads]]
            url = "https://deno.land/install.sh"
            env = { DENO_INSTALL = "/usr/local" }
            "#,
        )
        .unwrap();
        let file = config.rendered();
        assert!(file.contains(
            "RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates unzip && rm -rf /var/lib/apt/lists/* && curl -fsSL 'https://deno.land/install.sh' | DENO_INSTALL='/usr/local' sh"
        ));
    }

    #[test]
    fn download_without_env_pipes_to_sh() {
        let config: Config = toml::from_str(
            r#"
            [harness]
            name = "opencode"

            [[downloads]]
            url = "https://example.com/install.sh"
            "#,
        )
        .unwrap();
        let file = config.rendered();
        assert!(file.contains("curl -fsSL 'https://example.com/install.sh' | sh"));
    }
}
