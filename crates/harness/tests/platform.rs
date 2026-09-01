use std::path::Path;

use pithos_harness::platform::volume_spec;

#[test]
fn volume_spec_selects_mutability() {
    let source = Path::new("/tmp/opencode");
    assert_eq!(
        volume_spec(source, "/data", false),
        "/tmp/opencode:/data:rw"
    );
    assert_eq!(
        volume_spec(source, "/config", true),
        "/tmp/opencode:/config:ro"
    );
}

#[test]
fn volume_spec_handles_spaces() {
    let source = Path::new("/tmp/my data");
    assert_eq!(
        volume_spec(source, "/target", false),
        "/tmp/my data:/target:rw"
    );
}
