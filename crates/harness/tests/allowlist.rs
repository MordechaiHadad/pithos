use std::fs;
use std::process::Command;

use pithos_harness::{HarnessDef, registry};

fn command_args(command: &Command) -> Vec<String> {
    command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

fn write_file(dir: &std::path::Path, name: &str, contents: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    fs::write(&path, contents).unwrap();
    path
}

fn rendered_content(runtime: &std::path::Path) -> String {
    let session = runtime.join("session");
    let entry = fs::read_dir(&session)
        .unwrap()
        .next()
        .expect("sink must write exactly one file")
        .unwrap();
    fs::read_to_string(entry.path()).unwrap()
}

#[test]
fn sink_parses_from_toml() {
    let def = HarnessDef::from_toml_str(
        r#"
schema_version = 1
name = "example"

[allowlist]
target = "/home/agent/.example/settings.json"
format = "json"
"#,
    )
    .unwrap();
    assert!(def.allowlist.has_sink());
    assert_eq!(def.allowlist.target, "/home/agent/.example/settings.json");
}

#[test]
fn missing_sink_means_no_support() {
    let def = HarnessDef::from_toml_str(
        r#"
schema_version = 1
name = "example"
"#,
    )
    .unwrap();
    assert!(!def.allowlist.has_sink());
}

#[test]
fn embedded_harnesses_declare_absolute_file_sinks() {
    for name in ["opencode", "claude-code", "codex"] {
        let def = registry::find(name).unwrap_or_else(|| panic!("{name} must exist"));
        assert!(def.allowlist.has_sink(), "{name} has no allowlist sink");
        assert!(
            def.allowlist.target.starts_with('/'),
            "{name} sink target must be absolute"
        );
    }
}

#[test]
fn override_file_is_mounted_at_sink_target() {
    let dir = tempfile::tempdir().unwrap();
    let override_file = write_file(
        dir.path(),
        "sandbox.json",
        r#"{"permission": {"bash": {"git *": "allow"}}}"#,
    );
    let runtime = tempfile::tempdir().unwrap();
    let def = registry::find("opencode").expect("opencode must exist");
    let mut command = Command::new("podman");
    pithos_harness::mount::apply_mounts(
        &def,
        &mut command,
        "session",
        runtime.path(),
        Some(&override_file),
        false,
    )
    .unwrap();
    let args = command_args(&command);
    let volume = args
        .windows(2)
        .find(|pair| pair[0] == "--volume")
        .map(|pair| pair[1].clone())
        .expect("sink must add a volume");
    assert!(
        volume.ends_with("/home/agent/.config/opencode/opencode.json:ro"),
        "{volume}"
    );
    let rendered = rendered_content(runtime.path());
    assert!(rendered.contains("\"git *\""), "{rendered}");
}

#[test]
fn opencode_config_dir_mount_is_skipped_when_override_present() {
    let dir = tempfile::tempdir().unwrap();
    let override_file = write_file(dir.path(), "sandbox.json", r#"{"permission": "allow"}"#);
    let runtime = tempfile::tempdir().unwrap();
    let def = registry::find("opencode").expect("opencode must exist");
    let mut command = Command::new("podman");
    pithos_harness::mount::apply_mounts(
        &def,
        &mut command,
        "session",
        runtime.path(),
        Some(&override_file),
        false,
    )
    .unwrap();
    let args = command_args(&command).join(" ");
    assert!(
        !args.contains("/home/agent/.config/opencode:ro"),
        "ancestor config dir mount must be skipped: {args}"
    );
    assert!(
        args.contains("/home/agent/.config/opencode/opencode.json:ro"),
        "sink file must be mounted: {args}"
    );
}

#[test]
fn override_file_is_passed_through_verbatim() {
    let overlay = "{\"permissions\": {\"allow\": [\"Bash(git *)\"]}}\n";
    let def = HarnessDef::from_toml_str(
        r#"
schema_version = 1
name = "example"

[allowlist]
target = "/home/agent/.example/settings.json"
format = "json"
"#,
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let override_file = write_file(dir.path(), "sandbox.json", overlay);
    let runtime = tempfile::tempdir().unwrap();
    let mut command = Command::new("podman");
    pithos_harness::mount::apply_mounts(
        &def,
        &mut command,
        "session",
        runtime.path(),
        Some(&override_file),
        false,
    )
    .unwrap();
    assert_eq!(rendered_content(runtime.path()), overlay);
}

#[test]
fn toml_override_file_is_passed_through_verbatim() {
    let overlay = "[permissions.tight.network]\nenabled = true\n";
    let def = HarnessDef::from_toml_str(
        r#"
schema_version = 1
name = "example"

[allowlist]
target = "/home/agent/.example/config.toml"
format = "toml"
"#,
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let override_file = write_file(dir.path(), "sandbox.toml", overlay);
    let runtime = tempfile::tempdir().unwrap();
    let mut command = Command::new("podman");
    pithos_harness::mount::apply_mounts(
        &def,
        &mut command,
        "session",
        runtime.path(),
        Some(&override_file),
        false,
    )
    .unwrap();
    assert_eq!(rendered_content(runtime.path()), overlay);
}

#[test]
fn generated_mount_without_matching_sink_fails() {
    let def = HarnessDef::from_toml_str(
        r#"
schema_version = 1
name = "example"

[[mount]]
host = "generated.json"
target = "/home/agent/.example/generated.json"
type = "generated"
access = "ro"
host_base = "runtime"
"#,
    )
    .unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let mut command = Command::new("podman");
    let error = pithos_harness::mount::apply_mounts(
        &def,
        &mut command,
        "session",
        runtime.path(),
        None,
        false,
    )
    .unwrap_err();
    assert!(error.to_string().contains("generated mount"), "{error}");
}

#[test]
fn override_without_sink_fails() {
    let def = HarnessDef::from_toml_str(
        r#"
schema_version = 1
name = "example"
"#,
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let override_file = write_file(dir.path(), "sandbox.json", "{}");
    let runtime = tempfile::tempdir().unwrap();
    let mut command = Command::new("podman");
    let error = pithos_harness::mount::apply_mounts(
        &def,
        &mut command,
        "session",
        runtime.path(),
        Some(&override_file),
        false,
    )
    .unwrap_err();
    assert!(error.to_string().contains("does not declare"), "{error}");
}

fn parse_harness(toml: &str) -> pithos_harness::Harness {
    toml::from_str(toml).unwrap()
}

#[test]
fn bare_relative_path_is_rejected_with_prefix_hint() {
    let harness = parse_harness(
        r#"
name = "opencode"
command = ["opencode"]
sandbox_config = "sandbox.json"
"#,
    );
    let dir = tempfile::tempdir().unwrap();
    let error = harness.validate(dir.path()).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("config:"), "{message}");
    assert!(message.contains("cwd:"), "{message}");
}

#[test]
fn missing_override_file_fails_with_resolved_path() {
    let harness = parse_harness(
        r#"
name = "opencode"
command = ["opencode"]
sandbox_config = "config:missing.json"
"#,
    );
    let dir = tempfile::tempdir().unwrap();
    let error = harness.validate(dir.path()).unwrap_err();
    assert!(error.to_string().contains("not found"), "{error}");
}

#[test]
fn invalid_json_override_fails() {
    let dir = tempfile::tempdir().unwrap();
    write_file(dir.path(), "sandbox.json", "not json {{{");
    let harness = parse_harness(
        r#"
name = "opencode"
command = ["opencode"]
sandbox_config = "config:sandbox.json"
"#,
    );
    let error = harness.validate(dir.path()).unwrap_err();
    assert!(error.to_string().contains("not valid JSON"), "{error}");
}

#[test]
fn valid_override_passes_validation() {
    let dir = tempfile::tempdir().unwrap();
    write_file(dir.path(), "sandbox.json", r#"{"permission": "allow"}"#);
    let harness = parse_harness(
        r#"
name = "opencode"
command = ["opencode"]
sandbox_config = "config:sandbox.json"
"#,
    );
    harness.validate(dir.path()).unwrap();
}

#[test]
fn cwd_prefix_resolves_against_process_dir() {
    let dir = tempfile::tempdir().unwrap();
    write_file(dir.path(), "sandbox.json", r#"{"permission": "allow"}"#);
    let harness = parse_harness(&format!(
        r#"
name = "opencode"
command = ["opencode"]
sandbox_config = "cwd:{}"
"#,
        dir.path().join("sandbox.json").display()
    ));
    harness
        .validate(std::path::Path::new("/nonexistent-config-dir"))
        .unwrap();
}
