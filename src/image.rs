use eyre::{Result, WrapErr, bail};
use std::fs;
use std::process::Command;

use crate::config::{Config, Download, Toolchain};
use crate::sandbox::TempDir;

impl Config {
    #[tracing::instrument(skip(self), fields(image_tag = %self.image_tag))]
    pub(crate) fn build_image(&self) -> Result<()> {
        let context = TempDir::create("pithos-build")?;
        let config_digest = self.digest();
        tracing::debug!(config_digest, "building image");
        fs::write(context.0.join("Containerfile"), self.containerfile())
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
        let up_to_date = stored == self.digest();
        tracing::trace!(stored_digest = %stored, local_digest = %self.digest(), up_to_date, "image digest comparison");
        Ok(up_to_date)
    }

    fn digest(&self) -> String {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::hash::DefaultHasher::new();
        self.containerfile().hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    fn containerfile(&self) -> String {
        let mut output = format!("FROM {}\nWORKDIR {}\n", self.base_image, self.workspace);
        output.push_str(&self.harness.install());
        if let Some(line) = install_line(
            "apt-get update && apt-get install -y --no-install-recommends",
            " && rm -rf /var/lib/apt/lists/*",
            &self.install,
        ) {
            output.push_str(&line);
        }
        for toolchain in Toolchain::ALL {
            if toolchain.wanted(self) {
                output.push_str(&toolchain.install_block(self));
            }
        }
        if let Some(line) = install_line("npm install --global", "", &self.npm) {
            output.push_str(&line);
        }
        if let Some(line) = install_line(
            "npm install --global bun && bun install --global",
            "",
            &self.bun,
        ) {
            output.push_str(&line);
        }
        for download in &self.downloads {
            output.push_str(&format!("RUN {}\n", download.command()));
        }
        output.push_str("RUN chmod -R a+rwX /tmp\n");
        output.push_str("CMD ");
        output.push_str(&json_command(self.harness.command()));
        output.push('\n');
        output
    }
}

impl Toolchain {
    const ALL: [Toolchain; 2] = [Toolchain::Rust, Toolchain::Python];

    fn wanted(&self, config: &Config) -> bool {
        match self {
            Toolchain::Rust => config.toolchains.contains(self) || !config.cargo.is_empty(),
            Toolchain::Python => config.toolchains.contains(self) || !config.uv.is_empty(),
        }
    }

    fn install_block(&self, config: &Config) -> String {
        let listed = config.toolchains.contains(self);
        match self {
            Toolchain::Rust => {
                let mut block = String::from(
                    "ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo \
                     PATH=/usr/local/cargo/bin:$PATH\n",
                );
                block.push_str(
                    "RUN apt-get update && apt-get install -y --no-install-recommends \
                     curl ca-certificates gcc libc6-dev && rm -rf /var/lib/apt/lists/*\n",
                );
                block.push_str("RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y\n");
                if listed {
                    block.push_str("RUN rustup component add rust-analyzer\n");
                }
                if let Some(line) = install_line("cargo install", "", &config.cargo) {
                    block.push_str(&line);
                }
                block
            }
            Toolchain::Python => {
                let mut block = String::from(
                    "RUN apt-get update && apt-get install -y --no-install-recommends \
                     curl ca-certificates && rm -rf /var/lib/apt/lists/*\n",
                );
                block.push_str(
                    "RUN curl -LsSf https://astral.sh/uv/install.sh | \
                     UV_INSTALL_DIR=/usr/local/bin sh\n",
                );
                block.push_str(
                    "ENV PATH=/usr/local/bin:$PATH \
                     UV_TOOL_BIN_DIR=/usr/local/bin \
                     UV_TOOL_DIR=/usr/local/uv/tools \
                     UV_PYTHON_INSTALL_DIR=/usr/local/uv/python\n",
                );
                if listed {
                    block.push_str("RUN uv tool install ruff\n");
                }
                for tool in &config.uv {
                    let name = shell_quote(&tool.name);
                    if let Some(python) = &tool.python {
                        block.push_str(&format!(
                            "RUN uv tool install {name} --python {}\n",
                            shell_quote(python)
                        ));
                    } else {
                        block.push_str(&format!("RUN uv tool install {name}\n"));
                    }
                    if let Some(run) = &tool.run {
                        block.push_str(&format!("RUN {run}\n"));
                    }
                }
                if listed {
                    block.push_str("RUN npm install --global pyright\n");
                }
                block
            }
        }
    }
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
        fn with_rust_toolchain() -> Self {
            toml::from_str(
                r#"
                toolchains = ["rust"]

                [harness]
                name = "opencode"
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
    fn cargo_block_installs_rustup_then_crates() {
        let file = Config::with_cargo().containerfile();
        assert!(file.contains("ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo"));
        assert!(file.contains("RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y"));
        assert!(file.contains("RUN cargo install 'just'\n"));
        assert!(file.contains(
            "RUN apt-get update && apt-get install -y --no-install-recommends \
             curl ca-certificates gcc libc6-dev && rm -rf /var/lib/apt/lists/*\n"
        ));
    }

    #[test]
    fn every_line_is_a_valid_instruction() {
        let config: Config = toml::from_str(
            r#"
            toolchains = ["rust", "python"]
            install = ["git"]
            cargo = ["just"]
            npm = ["prettier"]
            bun = ["htmx"]
            uv = [{ name = "serena-agent", python = "3.13", run = "serena init" }]

            [harness]
            name = "opencode"

            [[downloads]]
            url = "https://example.com/install.sh"
            "#,
        )
        .unwrap();
        for line in config.containerfile().lines() {
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
        let file = config.containerfile();
        assert!(!file.contains("rustup"));
    }

    #[test]
    fn toolchain_flag_installs_toolchain_without_config_entry() {
        let config: Config = toml::from_str("[harness]\nname = \"opencode\"").unwrap();
        let config = config.with_toolchain(Some(Toolchain::Rust));
        let file = config.containerfile();
        assert!(file.contains("ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo"));
        assert!(file.contains("RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y"));
        assert!(file.contains("RUN rustup component add rust-analyzer\n"));
    }

    #[test]
    fn rust_toolchain_installs_rustup_analyzer_and_build_deps() {
        let file = Config::with_rust_toolchain().containerfile();
        assert!(file.contains("ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo"));
        assert!(file.contains("RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y"));
        assert!(file.contains("gcc libc6-dev"));
        assert!(file.contains("RUN rustup component add rust-analyzer\n"));
        assert!(!file.contains("cargo install"));
    }

    #[test]
    fn cargo_without_toolchain_skips_rust_analyzer() {
        let file = Config::with_cargo().containerfile();
        assert!(file.contains("RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y"));
        assert!(file.contains("gcc libc6-dev"));
        assert!(!file.contains("rust-analyzer"));
    }

    #[test]
    fn python_toolchain_installs_uv_ruff_and_pyright() {
        let file = Config::with_python_toolchain().containerfile();
        assert!(file.contains(
            "RUN curl -LsSf https://astral.sh/uv/install.sh | UV_INSTALL_DIR=/usr/local/bin sh"
        ));
        assert!(file.contains("RUN uv tool install ruff\n"));
        assert!(file.contains("RUN npm install --global pyright\n"));
    }

    #[test]
    fn uv_without_toolchain_skips_ruff_and_pyright() {
        let file = Config::with_uv().containerfile();
        assert!(file.contains("RUN uv tool install 'serena-agent' --python '3.13'\n"));
        assert!(!file.contains("ruff"));
        assert!(!file.contains("pyright"));
    }

    #[test]
    fn uv_block_installs_uv_and_tools() {
        let file = Config::with_uv().containerfile();
        assert!(file.contains(
            "RUN curl -LsSf https://astral.sh/uv/install.sh | UV_INSTALL_DIR=/usr/local/bin sh"
        ));
        assert!(file.contains("ENV PATH=/usr/local/bin:$PATH UV_TOOL_BIN_DIR=/usr/local/bin"));
        assert!(
            file.contains("RUN uv tool install 'serena-agent' --python '3.13'\nRUN serena init")
        );
        assert!(file.contains("RUN uv tool install 'plain-tool'\n"));
    }

    #[test]
    fn uv_block_absent_when_no_tools() {
        let config: Config = toml::from_str("[harness]\nname = \"opencode\"").unwrap();
        let file = config.containerfile();
        assert!(!file.contains("uv"));
    }

    #[test]
    fn containerfile_opens_tmp_to_all_users() {
        let config: Config = toml::from_str("[harness]\nname = \"opencode\"").unwrap();
        let file = config.containerfile();
        let chmod = file.find("RUN chmod -R a+rwX /tmp\n").unwrap();
        assert!(chmod < file.find("CMD ").unwrap());
    }

    #[test]
    fn config_digest_changes_with_tools() {
        let mut config = Config::with_uv();
        config.uv[0].python = Some("3.12".into());
        assert_ne!(config.digest(), Config::with_uv().digest());
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
        let file = config.containerfile();
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
        let file = config.containerfile();
        assert!(file.contains("curl -fsSL 'https://example.com/install.sh' | sh"));
    }
}
