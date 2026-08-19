use eyre::{Result, WrapErr, bail};
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::Networking;

pub(crate) const TABLE: &str = "pithos-egress";
pub(crate) const QUOTA_NAME: &str = "global_egress";

impl Networking {
    pub(crate) fn effective_whitelist(&self) -> Vec<String> {
        let mut hosts = Vec::new();
        if self.use_default_whitelist {
            hosts.extend(
                crate::config::DEFAULT_WHITELIST
                    .iter()
                    .map(|host| (*host).to_string()),
            );
        }
        hosts.extend(self.whitelist.iter().cloned());
        hosts
    }

    pub(crate) fn render_rules(
        &self,
        v4_whitelist: &[Ipv4Addr],
        v6_whitelist: &[Ipv6Addr],
    ) -> String {
        let mut rules = String::new();
        rules.push_str(&format!("table inet {TABLE} {{\n"));
        if let Some(quota_kb) = self.quota {
            rules.push_str(&format!(
                "  quota {QUOTA_NAME} {{ over {quota_kb} kbytes }}\n"
            ));
        }
        rules.push_str("\n  chain output {\n");
        rules.push_str("    type filter hook output priority filter; policy accept;\n");
        rules.push_str("    oifname \"lo\" accept\n");
        if !v4_whitelist.is_empty() {
            rules.push_str(&format!(
                "    ip daddr {{ {} }} tcp dport 443 accept\n",
                render_addr_list(v4_whitelist)
            ));
        }
        if !v6_whitelist.is_empty() {
            rules.push_str(&format!(
                "    ip6 daddr {{ {} }} tcp dport 443 accept\n",
                render_addr_list(v6_whitelist)
            ));
        }
        if let Some(payload_size_kb) = self.payload_size {
            rules.push_str(&format!(
                "    ct state established,related ct original bytes > {payload_size_kb} kbytes drop\n"
            ));
        }
        if self.quota.is_some() {
            rules.push_str(&format!("    quota name \"{QUOTA_NAME}\" drop\n"));
        }
        rules.push_str("  }\n}\n");
        rules
    }

    pub(crate) fn apply_to(&self, command: &mut Command) -> Result<()> {
        verify_host_support()?;
        let hosts = self.effective_whitelist();
        let (v4, v6) = resolve_hosts(&hosts);
        let rules = self.render_rules(&v4, &v6);
        let rules_path = write_rules(&rules, std::process::id())?;
        command.args(["--annotation", "pithos.networking=1"]);
        command.args([
            "--annotation",
            &format!("pithos.networking-rules={}", rules_path.display()),
        ]);
        Ok(())
    }
}

pub(crate) fn resolve_hosts(hosts: &[String]) -> (Vec<Ipv4Addr>, Vec<Ipv6Addr>) {
    let mut v4 = Vec::new();
    let mut v6 = Vec::new();
    for host in hosts {
        let addrs = match (host.as_str(), 443).to_socket_addrs() {
            Ok(addrs) => addrs.collect::<Vec<SocketAddr>>(),
            Err(_) => {
                eprintln!("warning: cannot resolve whitelist host {host}, skipping");
                continue;
            }
        };
        for addr in addrs {
            match addr.ip() {
                IpAddr::V4(ip) => v4.push(ip),
                IpAddr::V6(ip) => v6.push(ip),
            }
        }
    }
    v4.sort_unstable();
    v4.dedup();
    v6.sort_unstable();
    v6.dedup();
    (v4, v6)
}

pub(crate) fn write_rules(rules: &str, pid: u32) -> Result<PathBuf> {
    let directory = runtime_directory()?.join("pithos");
    fs::create_dir_all(&directory)
        .wrap_err_with(|| format!("cannot create {}", directory.display()))?;
    let path = directory.join(format!("networking-{pid}.nft"));
    fs::write(&path, rules).wrap_err_with(|| format!("cannot write {}", path.display()))?;
    Ok(path)
}

pub(crate) fn verify_host_support() -> Result<()> {
    let mut search = vec![
        PathBuf::from("/usr/share/containers/oci/hooks.d"),
        PathBuf::from("/etc/containers/oci/hooks.d"),
    ];
    if let Some(config_dir) = dirs::config_dir() {
        search.push(config_dir.join("containers/oci/hooks.d"));
    }
    if let Some(home) = dirs::home_dir() {
        search.push(home.join(".config/containers/oci/hooks.d"));
    }
    match registered_hook(&search) {
        Some((json_path, hook_path)) => {
            if !is_executable(&hook_path) {
                bail!(
                    "OCI hook script {} (referenced by {}) is missing or not executable; \
                     fix the path or chmod +x the script",
                    hook_path.display(),
                    json_path.display()
                )
            }
        }
        None => bail!(
            "no OCI hook registered for pithos networking. Install \
             host/oci-hooks.d/pithos-egress-cap.json and its hook script into one of: {}",
            search
                .iter()
                .map(|dir| dir.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
    if !nft_available() {
        bail!(
            "nftables not found (expected /usr/sbin/nft or nft on PATH); install with `sudo apt install nftables`"
        )
    }
    Ok(())
}

fn registered_hook(search: &[PathBuf]) -> Option<(PathBuf, PathBuf)> {
    for dir in search {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let json_path = entry.path();
            if json_path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let content = match fs::read_to_string(&json_path) {
                Ok(content) => content,
                Err(_) => continue,
            };
            let value: serde_json::Value = match serde_json::from_str(&content) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let matches = value
                .get("when")
                .and_then(|when| when.get("annotations"))
                .and_then(|annotations| annotations.get("pithos.networking"))
                .and_then(|annotation| annotation.as_str())
                == Some("1");
            if !matches {
                continue;
            }
            let hook_path = value
                .get("hook")
                .and_then(|hook| hook.get("path"))
                .and_then(|path| path.as_str())
                .map(PathBuf::from);
            if let Some(hook_path) = hook_path {
                return Some((json_path, hook_path));
            }
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|meta| meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn nft_available() -> bool {
    let mut candidates = vec![PathBuf::from("/usr/sbin/nft")];
    if let Some(path_env) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path_env).map(|dir| dir.join("nft")));
    }
    candidates.iter().any(|candidate| candidate.is_file())
}

fn runtime_directory() -> Result<PathBuf> {
    let directory = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", current_uid())));
    if !directory.is_dir() {
        bail!("runtime directory {} does not exist", directory.display());
    }
    Ok(directory)
}

fn current_uid() -> u32 {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8_lossy(&output.stdout).trim().parse().ok())
        .unwrap_or(1000)
}

fn render_addr_list(addrs: &[impl std::fmt::Display]) -> String {
    addrs
        .iter()
        .map(|addr| addr.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::sandbox::TempDir;

    fn v4(value: &str) -> Ipv4Addr {
        value.parse().unwrap()
    }

    fn v6(value: &str) -> Ipv6Addr {
        value.parse().unwrap()
    }

    fn networking(payload_size: Option<u64>, quota: Option<u64>) -> Networking {
        Networking {
            payload_size,
            quota,
            whitelist: Vec::new(),
            use_default_whitelist: true,
        }
    }

    #[test]
    fn renders_full_ruleset() {
        let rules = networking(Some(8), Some(102400))
            .render_rules(&[v4("1.2.3.4"), v4("5.6.7.8")], &[v6("2001:db8::1")]);
        let expected = r#"table inet pithos-egress {
  quota global_egress { over 102400 kbytes }

  chain output {
    type filter hook output priority filter; policy accept;
    oifname "lo" accept
    ip daddr { 1.2.3.4, 5.6.7.8 } tcp dport 443 accept
    ip6 daddr { 2001:db8::1 } tcp dport 443 accept
    ct state established,related ct original bytes > 8 kbytes drop
    quota name "global_egress" drop
  }
}
"#;
        assert_eq!(rules, expected);
    }

    #[test]
    fn renders_cap_only_ruleset() {
        let rules = networking(Some(8), None).render_rules(&[], &[]);
        assert_eq!(
            rules,
            r#"table inet pithos-egress {

  chain output {
    type filter hook output priority filter; policy accept;
    oifname "lo" accept
    ct state established,related ct original bytes > 8 kbytes drop
  }
}
"#
        );
    }

    #[test]
    fn renders_quota_only_ruleset() {
        let rules = networking(None, Some(512)).render_rules(&[], &[]);
        assert_eq!(
            rules,
            r#"table inet pithos-egress {
  quota global_egress { over 512 kbytes }

  chain output {
    type filter hook output priority filter; policy accept;
    oifname "lo" accept
    quota name "global_egress" drop
  }
}
"#
        );
    }

    #[test]
    fn renders_v4_and_v6_whitelist_independently() {
        let v4_only = networking(None, Some(10)).render_rules(&[v4("1.2.3.4")], &[]);
        assert!(v4_only.contains("ip daddr { 1.2.3.4 } tcp dport 443 accept"));
        assert!(!v4_only.contains("ip6 daddr"));

        let v6_only = networking(None, Some(10)).render_rules(&[], &[v6("2001:db8::1")]);
        assert!(!v6_only.contains("ip daddr"));
        assert!(v6_only.contains("ip6 daddr { 2001:db8::1 } tcp dport 443 accept"));
    }

    #[test]
    fn whitelist_rules_precede_cap_and_quota() {
        let rules =
            networking(Some(8), Some(102400)).render_rules(&[v4("1.2.3.4")], &[v6("2001:db8::1")]);
        let loopback = rules.find("oifname \"lo\"").unwrap();
        let v4_rule = rules.find("ip daddr").unwrap();
        let cap = rules.find("ct state").unwrap();
        let quota = rules.find("quota name").unwrap();
        assert!(loopback < v4_rule && v4_rule < cap && cap < quota);
    }

    #[test]
    fn effective_whitelist_combines_defaults_and_custom() {
        let networking = Networking {
            payload_size: None,
            quota: None,
            whitelist: vec!["proxy.example.com".to_string()],
            use_default_whitelist: true,
        };
        assert_eq!(
            networking.effective_whitelist(),
            vec![
                "opencode.ai".to_string(),
                "mcp.exa.ai".to_string(),
                "api.exa.ai".to_string(),
                "proxy.example.com".to_string(),
            ]
        );
    }

    #[test]
    fn effective_whitelist_can_disable_defaults() {
        let networking = Networking {
            payload_size: None,
            quota: None,
            whitelist: vec!["proxy.example.com".to_string()],
            use_default_whitelist: false,
        };
        assert_eq!(
            networking.effective_whitelist(),
            vec!["proxy.example.com".to_string()]
        );
    }

    #[test]
    fn registered_hook_finds_matching_annotation() {
        let dir = TempDir::create("pithos-hook-test").unwrap();
        fs::write(
            dir.0.join("pithos-egress-cap.json"),
            r#"{
              "version": "1.0.0",
              "hook": { "path": "/usr/local/bin/pithos-egress-cap.sh", "args": [] },
              "when": { "annotations": { "pithos.networking": "1" } },
              "stages": ["createRuntime"]
            }"#,
        )
        .unwrap();
        fs::write(
            dir.0.join("other.json"),
            r#"{
              "version": "1.0.0",
              "hook": { "path": "/usr/local/bin/other.sh", "args": [] },
              "when": { "annotations": { "some.other.annotation": "1" } },
              "stages": ["createRuntime"]
            }"#,
        )
        .unwrap();

        let (json_path, hook_path) = registered_hook(std::slice::from_ref(&dir.0)).unwrap();
        assert_eq!(
            json_path.file_name().unwrap().to_str().unwrap(),
            "pithos-egress-cap.json"
        );
        assert_eq!(
            hook_path,
            PathBuf::from("/usr/local/bin/pithos-egress-cap.sh")
        );
    }

    #[test]
    fn registered_hook_returns_none_without_match() {
        let dir = TempDir::create("pithos-hook-test-none").unwrap();
        fs::write(
            dir.0.join("other.json"),
            r#"{
              "version": "1.0.0",
              "hook": { "path": "/usr/local/bin/other.sh", "args": [] },
              "when": { "annotations": { "some.other.annotation": "1" } },
              "stages": ["createRuntime"]
            }"#,
        )
        .unwrap();
        assert!(registered_hook(std::slice::from_ref(&dir.0)).is_none());
    }
}
