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
install = "RUN echo hi\n"
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
install = "RUN echo hi\n"
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
