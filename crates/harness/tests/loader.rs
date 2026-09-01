use std::fs;
use tempfile::TempDir;

use pithos_harness::loader;

#[test]
fn loads_valid_harness_from_dir() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("my.toml"),
        r#"
schema_version = 1
name = "my"
install = "echo hi"
[[mount]]
host = ""
target = "/tmp"
type = "ephemeral"
access = "tmpfs"
host_base = "home"
"#,
    )
    .unwrap();
    let harnesses = loader::load_from_dir(dir.path());
    assert_eq!(harnesses.len(), 1);
    assert_eq!(harnesses[0].name, "my");
}

#[test]
fn ignores_invalid_file_but_loads_valid() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("bad.toml"), "not toml = [[[ ").unwrap();
    fs::write(
        dir.path().join("good.toml"),
        r#"
schema_version = 1
name = "good"
install = "echo hi"
[[mount]]
host = ""
target = "/tmp"
type = "ephemeral"
access = "tmpfs"
host_base = "home"
"#,
    )
    .unwrap();
    let harnesses = loader::load_from_dir(dir.path());
    assert_eq!(harnesses.len(), 1);
    assert_eq!(harnesses[0].name, "good");
}

#[test]
fn run_prefixed_install_is_rejected() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("legacy.toml"),
        r#"
schema_version = 1
name = "legacy"
install = "RUN echo hi\n"
"#,
    )
    .unwrap();
    let harnesses = loader::load_from_dir(dir.path());
    assert!(harnesses.is_empty());
}

#[test]
fn depends_on_entries_are_vetted() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("bad.toml"),
        r#"
schema_version = 1
name = "bad"
install = "echo hi"
depends_on = ["node"]
"#,
    )
    .unwrap();
    let harnesses = loader::load_from_dir(dir.path());
    assert!(harnesses.is_empty());
}
