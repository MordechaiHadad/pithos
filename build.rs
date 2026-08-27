use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let harness_dir = manifest_dir.join("harnesses");
    let output_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let output_path = output_dir.join("harnesses.rs");

    println!("cargo:rerun-if-changed={}", harness_dir.display());

    let mut files = fs::read_dir(&harness_dir)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", harness_dir.display()))
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("toml"))
        .collect::<Vec<_>>();
    files.sort();

    let mut generated = String::from("pub const EMBEDDED: &[(&str, &str)] = &[\n");
    for path in files {
        let name = path.file_stem().unwrap().to_string_lossy();
        let path = path
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        generated.push_str(&format!("    (\"{name}\", include_str!(\"{path}\")),\n"));
    }
    generated.push_str("];\n");
    fs::write(&output_path, generated).unwrap();
}
