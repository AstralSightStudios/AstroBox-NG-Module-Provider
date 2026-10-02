use super::*;
use anyhow::Context;

pub(super) fn add_manager_requirement(manifest: &mut ManifestV2) -> anyhow::Result<()> {
    if manifest.item.restype != ResourceTypeV2::ResPack {
        return Ok(());
    }
    if manifest.ext.is_null() {
        manifest.ext = serde_json::json!({});
    }
    let ext = manifest
        .ext
        .as_object_mut()
        .context("manifest ext must be an object")?;
    let bundles = ext
        .entry("bundledResources")
        .or_insert_with(|| serde_json::json!({}));
    let bundles = bundles
        .as_object_mut()
        .context("bundledResources must be an object")?;
    let required = bundles
        .entry("required")
        .or_insert_with(|| serde_json::json!([]));
    let required = required
        .as_array_mut()
        .context("bundledResources.required must be an array")?;
    if !required.iter().any(|entry| {
        entry["type"] == "resource"
            && entry["id"] == "ng.lst.corona"
            && entry
                .get("provider")
                .is_none_or(|provider| provider == "OfficialV2")
    }) {
        required.insert(
            0,
            serde_json::json!({"type":"resource","id":"ng.lst.corona","provider":"OfficialV2"}),
        );
    }
    Ok(())
}
