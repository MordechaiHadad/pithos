use serde_json::{Value, json};

use super::Allowlist;

pub fn claude_settings_translation(allowlist: &Allowlist, user_settings: Value) -> Value {
    merge_claude_settings(user_settings, allowlist)
}

fn merge_claude_settings(mut user_settings: Value, allowlist: &Allowlist) -> Value {
    let mut allow = Vec::new();
    let mut ask = Vec::new();
    let mut deny = Vec::new();
    let mut push = |verdict: &str, rule: String| match verdict {
        "allow" => allow.push(Value::String(rule)),
        "ask" => ask.push(Value::String(rule)),
        "deny" => deny.push(Value::String(rule)),
        other => tracing::warn!(verdict = other, %rule, "unknown allowlist verdict ignored"),
    };
    if let Some(Some(edit)) = allowlist.get("edit").map(Value::as_str) {
        for tool in ["Edit", "Write"] {
            push(edit, tool.to_string());
        }
    }
    if let Some(Some(patterns)) = allowlist.get("bash").map(Value::as_object) {
        for (pattern, verdict) in patterns {
            if let Some(verdict) = verdict.as_str() {
                push(verdict, format!("Bash({pattern})"));
            }
        }
    }
    user_settings["permissions"] = json!({ "allow": allow, "ask": ask, "deny": deny });
    user_settings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_keeps_user_keys_and_replaces_permissions() {
        let allowlist: Allowlist = [
            ("edit".to_string(), json!("deny")),
            ("bash".to_string(), json!({ "git *": "allow", "*": "ask" })),
        ]
        .into_iter()
        .collect();
        let user = json!({ "model": "opus", "permissions": { "allow": ["Stale"] } });
        let merged = merge_claude_settings(user, &allowlist);
        assert_eq!(merged["model"], "opus");
        assert_eq!(merged["permissions"]["allow"], json!(["Bash(git *)"]));
        assert_eq!(merged["permissions"]["ask"], json!(["Bash(*)"]));
        assert_eq!(merged["permissions"]["deny"], json!(["Edit", "Write"]));
    }
}
