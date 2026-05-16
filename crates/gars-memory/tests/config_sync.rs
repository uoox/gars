//! Drift guard: the shipped sample `assets/config.toml` must stay in sync with
//! the embedded `DEFAULT_CONFIG_TEMPLATE`. The only allowed difference is the
//! admin token placeholder.

use std::path::PathBuf;

#[test]
fn assets_config_matches_template() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = manifest_dir
        .join("..")
        .join("..")
        .join("assets")
        .join("config.toml");
    let on_disk =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let normalized = on_disk.replace("replace-me", "__ADMIN_TOKEN__");
    assert_eq!(
        normalized,
        gars_memory::DEFAULT_CONFIG_TEMPLATE,
        "assets/config.toml has drifted from DEFAULT_CONFIG_TEMPLATE \
         (regen: `awk` extract const, sed __ADMIN_TOKEN__ → replace-me)"
    );
}
