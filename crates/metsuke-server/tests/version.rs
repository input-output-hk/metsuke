//! Nothing else in the tree would notice `build.rs` reading the wrong
//! version, or none at all.

#[test]
fn client_version_is_the_agent_crates_version() {
    let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/../metsuke/Cargo.toml");
    let parsed: toml::Table = std::fs::read_to_string(manifest).unwrap().parse().unwrap();
    let version = parsed["package"]["version"].as_str().unwrap();
    assert_eq!(metsuke_server::CLIENT_VERSION, version);
}
