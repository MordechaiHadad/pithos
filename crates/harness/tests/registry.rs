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
}

#[test]
fn available_names_lists_all_embedded() {
    let names = registry::available_names();
    assert!(names.len() >= 2);
    assert!(names.windows(2).all(|w| w[0] <= w[1]));
}
