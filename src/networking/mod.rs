pub(crate) mod enforcement;

use eyre::Result;
use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::config::Networking;

pub(crate) const TABLE: &str = "pithos-egress";
pub(crate) const QUOTA_NAME: &str = "global_egress";

/// Environment variable carrying the rendered nftables ruleset into the
/// container, where its entrypoint loads it before dropping privileges.
pub(crate) const RULES_ENV: &str = "PITHOS_EGRESS_RULES";

/// Private IPv4 ranges that must be dropped while `block_private` is set:
/// the three RFC1918 blocks plus IPv4 link-local (RFC3927).
///
/// Shared with the enforcement check so the ruleset and its verification can
/// never drift apart.
pub(crate) const PRIVATE_V4_RANGES: [&str; 4] = [
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "169.254.0.0/16",
];

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
        if self.block_private {
            let private_v4 = PRIVATE_V4_RANGES.join(", ");
            rules.push_str(&format!(
                "    ip daddr {{ {private_v4} }} tcp dport 53 accept\n"
            ));
            rules.push_str(&format!(
                "    ip daddr {{ {private_v4} }} udp dport 53 accept\n"
            ));
            rules.push_str(&format!("    ip daddr {{ {private_v4} }} drop\n"));
            rules.push_str("    ip6 daddr { fc00::/7, fe80::/10 } tcp dport 53 accept\n");
            rules.push_str("    ip6 daddr { fc00::/7, fe80::/10 } udp dport 53 accept\n");
            rules.push_str("    ip6 daddr { fc00::/7, fe80::/10 } drop\n");
        }
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
            let payload_bytes = payload_size_kb.saturating_mul(1024);
            rules.push_str(&format!(
                "    ct state established,related ct original bytes > {payload_bytes} drop\n"
            ));
        }
        if self.quota.is_some() {
            rules.push_str(&format!("    quota name \"{QUOTA_NAME}\" drop\n"));
        }
        rules.push_str("  }\n}\n");
        rules
    }

    #[tracing::instrument(skip(self, command))]
    pub(crate) fn apply_to(&self, command: &mut std::process::Command) -> Result<()> {
        if !self.enabled {
            tracing::debug!("networking disabled by configuration; skipping egress rules");
            return Ok(());
        }
        remove_legacy_hook_install();
        let hosts = self.effective_whitelist();
        let (v4, v6) = resolve_hosts(&hosts);
        let rules = serialize_rules(&self.render_rules(&v4, &v6));
        tracing::debug!(
            whitelist = ?hosts,
            v4_addresses = v4.len(),
            v6_addresses = v6.len(),
            rules_bytes = rules.len(),
            "delivering egress networking rules to the container entrypoint"
        );
        command.env(RULES_ENV, &rules);
        Ok(())
    }
}

/// Removes hook files written by older pithos versions that loaded rules
/// through OCI hooks on the host. Best effort: leftovers only cause
/// confusion, not harm.
fn remove_legacy_hook_install() {
    if let Some(dir) = dirs::data_local_dir() {
        let _ = fs::remove_dir_all(dir.join("pithos").join("hooks.d"));
    }
    if let Some(dir) = dirs::config_dir() {
        let _ = fs::remove_file(
            dir.join("containers")
                .join("oci")
                .join("hooks.d")
                .join("pithos-egress-cap.json"),
        );
    }
}

/// Renders a pretty multi-line nft ruleset as a single line with no double
/// quotes, so it can travel through an environment variable into the
/// container entrypoint regardless of which filesystem it runs on.
///
/// Statements are `;`-terminated; opening braces are kept bare because the
/// following statement supplies the separator, while closing braces are
/// terminated like any other statement (`nft` requires a separator between
/// sibling blocks, e.g. between a quota declaration and the next chain).
pub(crate) fn serialize_rules(rules: &str) -> String {
    let mut statements = Vec::new();
    for line in rules.lines() {
        let line = line.trim().replace('"', "");
        if line.is_empty() {
            continue;
        }
        if line.ends_with('{') || line.ends_with(';') {
            statements.push(line);
        } else {
            statements.push(format!("{line};"));
        }
    }
    statements.join(" ")
}

/// Wall-clock budget for resolving the whole whitelist. Every host resolves
/// on its own thread, so a slow or unreachable resolver costs this ceiling
/// once instead of once per host.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(3);

pub(crate) fn resolve_hosts(hosts: &[String]) -> (Vec<Ipv4Addr>, Vec<Ipv6Addr>) {
    let started = Instant::now();
    let (tx, rx) = mpsc::channel::<(String, io::Result<Vec<SocketAddr>>)>();
    for host in hosts {
        let tx = tx.clone();
        let host = host.clone();
        std::thread::spawn(move || {
            let addrs = (host.as_str(), 443).to_socket_addrs().map(|addrs| {
                // Collect eagerly so resolution happens on this thread
                // rather than lazily after it detaches.
                addrs.collect::<Vec<SocketAddr>>()
            });
            let _ = tx.send((host, addrs));
        });
    }
    drop(tx);
    let mut v4 = Vec::new();
    let mut v6 = Vec::new();
    for _ in 0..hosts.len() {
        let remaining = RESOLVE_TIMEOUT.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break;
        }
        match rx.recv_timeout(remaining) {
            Ok((host, Ok(addrs))) => {
                tracing::trace!(host, addresses = addrs.len(), "resolved whitelist host");
                for addr in addrs {
                    match addr.ip() {
                        IpAddr::V4(ip) => v4.push(ip),
                        IpAddr::V6(ip) => v6.push(ip),
                    }
                }
            }
            Ok((host, Err(_))) => {
                eprintln!("warning: cannot resolve whitelist host {host}, skipping");
            }
            Err(mpsc::RecvTimeoutError::Timeout) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                break;
            }
        }
    }
    if started.elapsed() >= RESOLVE_TIMEOUT {
        tracing::warn!(
            elapsed_ms = started.elapsed().as_millis() as u64,
            "whitelist resolution hit its time budget; unresolved hosts are skipped"
        );
    }
    v4.sort_unstable();
    v4.dedup();
    v6.sort_unstable();
    v6.dedup();
    (v4, v6)
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

    #[test]
    fn resolves_localhost_and_skips_unknown_hosts() {
        let (v4, _v6) = resolve_hosts(&[
            "localhost".to_string(),
            "pithos-does-not-exist.example".to_string(),
        ]);
        assert!(
            v4.contains(&Ipv4Addr::LOCALHOST),
            "expected localhost in {v4:?}"
        );
    }

    #[test]
    fn rendered_rules_cover_every_private_range() {
        let mut networking = networking(None, None);
        networking.block_private = true;
        let rules = networking.render_rules(&[], &[]);
        assert!(PRIVATE_V4_RANGES.iter().all(|range| rules.contains(range)));
        assert_eq!(rules.matches("} drop").count(), 2);
    }

    fn v4(value: &str) -> Ipv4Addr {
        value.parse().unwrap()
    }

    fn v6(value: &str) -> Ipv6Addr {
        value.parse().unwrap()
    }

    fn networking(payload_size: Option<u64>, quota: Option<u64>) -> Networking {
        Networking {
            enabled: true,
            payload_size,
            quota,
            whitelist: Vec::new(),
            use_default_whitelist: true,
            block_private: false,
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
    ct state established,related ct original bytes > 8192 drop
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
    ct state established,related ct original bytes > 8192 drop
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
    fn private_ranges_drop_with_dns_exception() {
        let mut networking = networking(Some(8), Some(102400));
        networking.block_private = true;
        let rules = networking.render_rules(&[], &[]);
        assert!(rules.contains("ip daddr { 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, 169.254.0.0/16 } tcp dport 53 accept"));
        assert!(rules.contains("udp dport 53 accept"));
        assert!(rules.contains(
            "ip daddr { 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, 169.254.0.0/16 } drop"
        ));
        assert!(rules.contains("ip6 daddr { fc00::/7, fe80::/10 } drop"));

        let drops = rules.matches("} drop").count();
        let dns_accepts = rules.matches("dport 53 accept").count();
        assert_eq!(drops, 2);
        assert_eq!(dns_accepts, 4);

        // Private drops precede the whitelist accepts so they win conflicts.
        let first_private = rules.find("169.254.0.0/16").unwrap();
        if let Some(whitelist_pos) = rules.find("tcp dport 443 accept") {
            assert!(first_private < whitelist_pos);
        }
    }

    #[test]
    fn disabled_networking_is_the_default_for_rendering() {
        let rules = networking(Some(8), Some(102400)).render_rules(&[], &[]);
        assert!(!rules.contains("169.254.0.0/16"));
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
            enabled: true,
            payload_size: None,
            quota: None,
            whitelist: vec!["proxy.example.com".to_string()],
            use_default_whitelist: true,
            block_private: false,
        };
        assert_eq!(
            networking.effective_whitelist(),
            vec![
                "opencode.ai".to_string(),
                "mcp.exa.ai".to_string(),
                "api.exa.ai".to_string(),
                "api.parallel.ai".to_string(),
                "search.parallel.ai".to_string(),
                "task-mcp.parallel.ai".to_string(),
                "api.tavily.com".to_string(),
                "api.search.brave.com".to_string(),
                "google.serper.dev".to_string(),
                "api.anthropic.com".to_string(),
                "statsig.anthropic.com".to_string(),
                "proxy.example.com".to_string(),
            ]
        );
    }

    #[test]
    fn effective_whitelist_can_disable_defaults() {
        let networking = Networking {
            enabled: true,
            payload_size: None,
            quota: None,
            whitelist: vec!["proxy.example.com".to_string()],
            use_default_whitelist: false,
            block_private: false,
        };
        assert_eq!(
            networking.effective_whitelist(),
            vec!["proxy.example.com".to_string()]
        );
    }

    #[test]
    fn serialize_rules_produces_quote_free_single_line_program() {
        let rules = networking(Some(8), Some(102400)).render_rules(&[], &[]);
        let serialized = serialize_rules(&rules);
        assert!(!serialized.contains('\n'));
        assert!(!serialized.contains('"'));
        assert!(!serialized.contains("{;"));
        assert!(!serialized.contains('\\'));
        assert!(serialized.starts_with("table inet pithos-egress {"));
        assert!(serialized.ends_with("};"));
        assert!(serialized.contains("oifname lo accept;"));
        assert!(serialized.contains("over 102400 kbytes };"));
        assert!(serialized.contains("ct original bytes > 8192 drop;"));
        assert!(serialized.contains("quota name global_egress drop;"));
        assert!(serialized.contains("chain output {"));
    }
}
