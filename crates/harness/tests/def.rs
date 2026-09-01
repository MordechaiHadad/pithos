use pithos_harness::HarnessDef;

#[test]
fn accepts_multiple_mount_entries_and_closed_types() {
    let definition = HarnessDef::from_toml_str(
        r#"
schema_version = 1
name = "example"

[[mount]]
host = "credentials.json"
target = "/home/agent/.credentials.json"
type = "credentials"
access = "ro"
host_base = "home"

[[mount]]
host = "data.json"
target = "/home/agent/.data.json"
type = "state"
access = "pinned"
host_base = "data:example"
"#,
    )
    .unwrap();
    assert_eq!(definition.mounts.len(), 2);
}

#[test]
fn rejects_unknown_mount_type() {
    let error = HarnessDef::from_toml_str(
        r#"
schema_version = 1
name = "example"

[[mount]]
host = "data"
target = "/data"
type = "unknown"
access = "ro"
host_base = "home"
"#,
    )
    .unwrap_err();
    assert!(error.to_string().contains("unknown"));
}

#[test]
fn parses_keychain_and_file_credentials_with_platforms() {
    let def = HarnessDef::from_toml_str(
        r#"
schema_version = 1
name = "example"

[[credential]]
source = "keychain:Claude Code-credentials"
target = "/home/agent/.claude/.credentials.json"
platforms = ["macos"]
on_missing = "warn"

[[credential]]
source = "file:.claude/.credentials.json"
target = "/home/agent/.claude/.credentials.json"
host_base = "home"
on_missing = "warn"
"#,
    )
    .unwrap();
    assert_eq!(def.credentials.len(), 2);
    assert_eq!(
        def.credentials[0].source,
        "keychain:Claude Code-credentials"
    );
    assert_eq!(def.credentials[1].source, "file:.claude/.credentials.json");
}

#[test]
fn parses_file_credential_with_data_host_base() {
    let def = HarnessDef::from_toml_str(
        r#"
schema_version = 1
name = "example"

[[credential]]
source = "file:auth.json"
target = "/home/agent/.local/share/opencode/auth.json"
host_base = "data:opencode"
on_missing = "warn"
"#,
    )
    .unwrap();
    assert_eq!(def.credentials.len(), 1);
    assert_eq!(
        def.credentials[0].host_base,
        pithos_harness::HostBase::Data("opencode".into())
    );
}

#[test]
fn rejects_invalid_credential_source() {
    let def = HarnessDef::from_toml_str(
        r#"
schema_version = 1
name = "example"

[[credential]]
source = "invalid:foo"
target = "/home/agent/.foo"
"#,
    )
    .unwrap();
    assert_eq!(def.credentials.len(), 1);
}
