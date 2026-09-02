use pithos_harness::registry;

#[test]
fn embedded_contains_opencode_and_claude() {
    let names = registry::available_names();
    assert!(names.contains(&"opencode".to_string()));
    assert!(names.contains(&"claude-code".to_string()));
}

#[test]
fn find_returns_cloned() {
    assert!(registry::find("opencode").is_some());
    assert!(registry::find("nonexistent").is_none());
}

#[test]
fn find_returns_correct_definition() {
    let def = registry::find("opencode").expect("opencode must exist");
    assert_eq!(def.name, "opencode");
    assert!(!def.install.is_empty());
    assert_eq!(def.depends_on, [pithos_harness::HarnessDependency::Npm]);
}

#[test]
fn embedded_installs_are_plain_shell_commands() {
    for def in registry::all_harnesses() {
        let first_word = def
            .install
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        assert_ne!(first_word, "RUN", "harness {} bakes a RUN prefix", def.name);
    }
}

#[test]
fn available_names_lists_all_embedded() {
    let names = registry::available_names();
    assert!(names.len() >= 2);
    assert!(names.windows(2).all(|w| w[0] <= w[1]));
}

#[test]
fn codex_mounts_parent_tmpfs_before_pinned_children() {
    let def = registry::find("codex").expect("embedded codex definition parses");
    let first = def.mounts.first().expect("codex has mounts");
    assert_eq!(first.target, "/home/agent/.codex");
    assert_eq!(first.mount_type, pithos_harness::MountType::State);
    assert_eq!(first.access, pithos_harness::Access::PinnedDir);
    let state_targets: Vec<_> = def
        .mounts
        .iter()
        .filter(|m| m.mount_type == pithos_harness::MountType::State)
        .map(|m| m.target.as_str())
        .collect();
    assert_eq!(state_targets, ["/home/agent/.codex"]);
    let ephemeral_targets: Vec<_> = def
        .mounts
        .iter()
        .filter(|m| m.mount_type == pithos_harness::MountType::Ephemeral)
        .map(|m| m.target.as_str())
        .collect();
    assert_eq!(
        ephemeral_targets,
        [
            "/home/agent/.codex/log",
            "/home/agent/.codex/cache",
            "/home/agent/.codex/tmp",
            "/home/agent/.codex/.tmp",
        ]
    );
    assert!(
        def.mounts.iter().all(|m| !m.target.contains("logs_")),
        "no log DB mounts"
    );
}
