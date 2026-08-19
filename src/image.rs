use eyre::{Result, WrapErr, bail};
use std::fs;
use std::process::Command;

use crate::config::{Config, Download};
use crate::sandbox::TempDir;

impl Config {
    pub(crate) fn build_image(&self) -> Result<()> {
        let context = TempDir::create("pithos-build")?;
        let config_digest = self.digest();
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
            return Ok(false);
        }
        let stored = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        Ok(stored == self.digest())
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
        if !self.cargo.is_empty() {
            output.push_str(
                "ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo \
                 PATH=/usr/local/cargo/bin:$PATH\n",
            );
            output.push_str(
                "RUN apt-get update && apt-get install -y --no-install-recommends \
                 curl ca-certificates && rm -rf /var/lib/apt/lists/*\n",
            );
            output.push_str("RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y\n");
            if let Some(line) = install_line("cargo install", "", &self.cargo) {
                output.push_str(&line);
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
        if !self.uv.is_empty() {
            output.push_str(
                "RUN apt-get update && apt-get install -y --no-install-recommends \
                 curl ca-certificates && rm -rf /var/lib/apt/lists/*\n",
            );
            output.push_str(
                "RUN curl -LsSf https://astral.sh/uv/install.sh | \
                 UV_INSTALL_DIR=/usr/local/bin sh\n",
            );
            output.push_str(
                "ENV PATH=/usr/local/bin:$PATH \
                 UV_TOOL_BIN_DIR=/usr/local/bin \
                 UV_TOOL_DIR=/usr/local/uv/tools \
                 UV_PYTHON_INSTALL_DIR=/usr/local/uv/python\n",
            );
            for tool in &self.uv {
                let name = shell_quote(&tool.name);
                if let Some(python) = &tool.python {
                    output.push_str(&format!(
                        "RUN uv tool install {name} --python {}\n",
                        shell_quote(python)
                    ));
                } else {
                    output.push_str(&format!("RUN uv tool install {name}\n"));
                }
                if let Some(run) = &tool.run {
                    output.push_str(&format!("RUN {run}\n"));
                }
            }
        }
        for download in &self.downloads {
            output.push_str(&format!("RUN {}\n", download.command()));
        }
        output.push_str("CMD ");
        output.push_str(&json_command(self.harness.command()));
        output.push('\n');
        output
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
    }

    #[test]
    fn cargo_block_absent_when_no_crates() {
        let config: Config = toml::from_str("[harness]\nname = \"opencode\"").unwrap();
        let file = config.containerfile();
        assert!(!file.contains("rustup"));
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
