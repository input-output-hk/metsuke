//! ADR 0006 wants the agent's version embedded at server build, and this
//! server does not depend on the agent crate (`metsuke-wire` says why), so
//! the version comes out of the manifest.

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets it"))
        .join("../metsuke/Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", manifest.display()));
    let parsed: toml::Table = text
        .parse()
        .unwrap_or_else(|error| panic!("cannot parse {}: {error}", manifest.display()));
    let version = parsed
        .get("package")
        .and_then(|package| package.get("version"))
        .and_then(|version| version.as_str())
        .unwrap_or_else(|| panic!("{} has no package.version", manifest.display()));
    println!("cargo::rerun-if-changed={}", manifest.display());
    println!("cargo::rustc-env=CLIENT_VERSION={version}");
}
