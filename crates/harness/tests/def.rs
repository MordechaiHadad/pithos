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
