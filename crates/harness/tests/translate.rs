use pithos_harness::translate::claude_settings_translation;
use serde_json::json;

#[test]
fn merge_keeps_user_keys_and_replaces_permissions() {
    let allowlist = [
        ("edit".to_string(), json!("deny")),
        ("bash".to_string(), json!({ "git *": "allow", "*": "ask" })),
    ]
    .into_iter()
    .collect();
    let user = json!({ "model": "opus", "permissions": { "allow": ["Stale"] } });
    let merged = claude_settings_translation(&allowlist, user);
    assert_eq!(merged["model"], "opus");
    assert_eq!(merged["permissions"]["allow"], json!(["Bash(git *)"]));
    assert_eq!(merged["permissions"]["ask"], json!(["Bash(*)"]));
    assert_eq!(merged["permissions"]["deny"], json!(["Edit", "Write"]));
}

#[test]
fn empty_allowlist_produces_empty_permissions() {
    let allowlist = Default::default();
    let user = json!({ "model": "sonnet" });
    let merged = claude_settings_translation(&allowlist, user);
    assert_eq!(merged["permissions"]["allow"], json!([]));
    assert_eq!(merged["permissions"]["ask"], json!([]));
    assert_eq!(merged["permissions"]["deny"], json!([]));
}

#[test]
fn claude_translation_ignores_unknown_verdict() {
    let allowlist = [("edit".to_string(), json!("unknown"))]
        .into_iter()
        .collect();
    let user = json!({});
    let merged = claude_settings_translation(&allowlist, user);
    assert_eq!(merged["permissions"]["allow"], json!([]));
}
