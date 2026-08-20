use eyre::Result;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
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
        crate::platform::verify_networking_support()?;
        let hosts = self.effective_whitelist();
        let (v4, v6) = resolve_hosts(&hosts);
        let rules = serialize_rules(&self.render_rules(&v4, &v6));
        command.args(["--annotation", "pithos.networking=1"]);
        command.args(["--annotation", &format!("pithos.networking-rules={rules}")]);
        Ok(())
    }
}

/// Renders a pretty multi-line nft ruleset as a single line with no double
/// quotes, so it can be embedded in an OCI annotation value and extracted by
/// the hook regardless of which filesystem the hook runs on.
///
/// Statements are `;`-terminated; opening and closing braces are kept bare, so
/// the output is the canonical compact form nft accepts (`nft list ruleset`
/// minified) and, crucially, never emits `{;`, which nft rejects.
pub(crate) fn serialize_rules(rules: &str) -> String {
    let mut statements = Vec::new();
    for line in rules.lines() {
        let line = line.trim().replace('"', "");
        if line.is_empty() {
            continue;
        }
        if line.ends_with('{') || line.ends_with(';') || line.ends_with('}') {
            statements.push(line);
        } else {
            statements.push(format!("{line};"));
        }
    }
    statements.join(" ")
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
    fn serialize_rules_produces_quote_free_single_line_program() {
        let rules = networking(Some(8), Some(102400)).render_rules(&[], &[]);
        let serialized = serialize_rules(&rules);
        assert!(!serialized.contains('\n'));
        assert!(!serialized.contains('"'));
        assert!(!serialized.contains("{;"));
        assert!(!serialized.contains('\\'));
        assert!(serialized.starts_with("table inet pithos-egress {"));
        assert!(serialized.ends_with('}'));
        assert!(serialized.contains("oifname lo accept;"));
        assert!(serialized.contains("quota name global_egress drop"));
        assert!(serialized.contains("chain output {"));
    }
}
