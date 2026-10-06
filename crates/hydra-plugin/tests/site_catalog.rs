//! Checks the official website catalog against Hydra's distribution index.

use hya_plugin::distribution::parse_index;

#[test]
fn official_site_index_matches_catalog_and_trusted_publisher() {
    let index = parse_index(include_bytes!("../../../docs/plugins.json"), None, None).unwrap();
    let catalog: serde_json::Value =
        serde_json::from_str(include_str!("../../../docs/plugins.json")).unwrap();
    let plugins = catalog["plugins"].as_array().unwrap();
    assert_eq!(index.plugins.len(), plugins.len());
    let key = include_str!("../../../plugins/official.pub")
        .lines()
        .find(|line| !line.starts_with("untrusted comment:"))
        .unwrap();
    for entry in index.plugins {
        let plugin = plugins
            .iter()
            .find(|plugin| plugin["id"] == entry.id)
            .unwrap();
        assert_eq!(plugin["name"], entry.name);
        assert_eq!(plugin["version"], entry.version);
        assert_eq!(plugin["download"], entry.package);
        assert_eq!(entry.is_official, plugin["is_official"].as_bool().unwrap());
        assert_eq!(entry.sha256.as_deref(), plugin["sha256"].as_str());
        assert_eq!(
            entry.publisher_key.as_deref(),
            plugin["publisher_key"].as_str()
        );
        if entry.is_official && entry.publisher_key.is_some() {
            assert_eq!(entry.publisher_key.as_deref(), Some(key));
        }
        let image = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs")
            .join(plugin["image"].as_str().unwrap());
        assert!(image.is_file());
    }
}
