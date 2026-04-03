use std::path::Path;

fn main() {
    // Read ZEROFS_VERSION from repo root and expose it as a compile-time env var.
    let version_file = Path::new("../../ZEROFS_VERSION");

    println!("cargo:rerun-if-changed={}", version_file.display());

    if let Ok(contents) = std::fs::read_to_string(version_file) {
        let version = contents.trim();
        println!("cargo:rustc-env=ZEROFS_VERSION={version}");
    } else {
        panic!(
            "ZEROFS_VERSION file not found at repo root. \
             Expected at: {}",
            version_file.display()
        );
    }
}
