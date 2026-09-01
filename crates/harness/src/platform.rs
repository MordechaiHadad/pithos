use std::path::Path;

pub fn volume_spec(source: &Path, target: &str, read_only: bool) -> String {
    let mode = if read_only { "ro" } else { "rw" };
    format!("{}:{target}:{mode}", source.display())
}
