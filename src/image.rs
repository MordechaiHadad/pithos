use eyre::{Result, WrapErr, bail};
use std::fs;
use std::process::Command;

use crate::config::{Config, Download};
use crate::sandbox::TempDir;

pub(crate) fn build_image(config: &Config) -> Result<()> {
    let context = TempDir::create("pithos-build")?;
    let config_digest = config_digest(config);
    fs::write(context.0.join("Containerfile"), containerfile(config))
        .wrap_err("cannot write Containerfile")?;
    let status = Command::new("podman")
        .args([
            "build",
            "--tag",
            &config.image_tag,
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

pub(crate) fn image_up_to_date(config: &Config) -> Result<bool> {
    let output = Command::new("podman")
        .args([
            "image",
            "inspect",
            "--format",
            "{{ index .Labels \"pithos.config\" }}",
            &config.image_tag,
        ])
        .output()
        .wrap_err("could not inspect podman image")?;
    if !output.status.success() {
        return Ok(false);
    }
    let stored = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    Ok(stored == config_digest(config))
}

fn config_digest(config: &Config) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    containerfile(config).hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn containerfile(config: &Config) -> String {
    let mut output = format!("FROM {}\nWORKDIR {}\n", config.base_image, config.workspace);
    output.push_str(&config.harness.install());
    if let Some(line) = install_line(
        "apt-get update && apt-get install -y --no-install-recommends",
        " && rm -rf /var/lib/apt/lists/*",
        &config.install,
    ) {
        output.push_str(&line);
    }
    if !config.cargo.is_empty() {
        output.push_str("RUN apt-get update && apt-get install -y --no-install-recommends cargo && rm -rf /var/lib/apt/lists/*\n");
        if let Some(line) = install_line("cargo install", "", &config.cargo) {
            output.push_str(&line);
        }
    }
    if let Some(line) = install_line("npm install --global", "", &config.npm) {
        output.push_str(&line);
    }
    if let Some(line) = install_line(
        "npm install --global bun && bun install --global",
        "",
        &config.bun,
    ) {
        output.push_str(&line);
    }
    if !config.uv.is_empty() {
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
        for tool in &config.uv {
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
    for download in &config.downloads {
        output.push_str(&format!("RUN {}\n", download_command(download)));
    }
    output.push_str("CMD ");
    output.push_str(&json_command(config.harness.command()));
    output.push('\n');
    output
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

fn download_command(download: &Download) -> String {
    let env = download
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
        shell_quote(&download.url)
    )
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

    fn config_with_uv() -> Config {
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

    #[test]
    fn uv_block_installs_uv_and_tools() {
        let file = containerfile(&config_with_uv());
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
        let file = containerfile(&config);
        assert!(!file.contains("uv"));
    }

    #[test]
    fn config_digest_changes_with_tools() {
        let config = config_with_uv();
        let mut config = config;
        config.uv[0].python = Some("3.12".into());
        assert_ne!(config_digest(&config), config_digest(&config_with_uv()));
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
        let file = containerfile(&config);
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
        let file = containerfile(&config);
        assert!(file.contains("curl -fsSL 'https://example.com/install.sh' | sh"));
    }
}
