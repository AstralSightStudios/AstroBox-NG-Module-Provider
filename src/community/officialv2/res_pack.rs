use super::*;
use anyhow::Context;

const MAX_SAFE_VERSION_CODE: u64 = 9_007_199_254_740_991;

impl OfficialV2Provider {
    /// Update-check lookup for packs: exact themeId, explicit pack type, and
    /// only the requested device or default download (never an arbitrary one).
    pub async fn resolve_res_pack_download_entry(
        &self,
        theme_id: &str,
        device: &str,
    ) -> anyhow::Result<ManifestDownloadV2> {
        let index = self.catalog.load().index.clone();
        let item = find_pack(&index, theme_id)?;
        let manifest = self
            .get_manifest(&item.repo_owner, &item.repo_name, &item.repo_commit_hash)
            .await
            .with_context(|| format!("failed to fetch resource pack manifest for {theme_id}"))?;
        let mut entry = select_pack_download(&manifest, theme_id, device)?;
        if entry.display_name.is_none() {
            entry.display_name = self.device_map_id_to_name(device);
        }
        let raw_url = self.download_raw_url(item, &entry);
        entry.url = Some(self.cdn.load_full().convert_url(&raw_url));
        Ok(entry)
    }
}

fn find_pack<'a>(index: &'a [IndexV2], theme_id: &str) -> anyhow::Result<&'a IndexV2> {
    index
        .iter()
        .find(|item| item.id == theme_id && item.restype == ResourceTypeV2::ResPack)
        .ok_or_else(|| anyhow!("resource pack not found by exact themeId `{theme_id}`"))
}

fn select_pack_download(
    manifest: &ManifestV2,
    theme_id: &str,
    device: &str,
) -> anyhow::Result<ManifestDownloadV2> {
    anyhow::ensure!(
        manifest.item.id == theme_id && manifest.item.restype == ResourceTypeV2::ResPack,
        "manifest is not resource pack `{theme_id}`"
    );
    let entry = manifest
        .downloads
        .get(device)
        .or_else(|| manifest.downloads.get("default"))
        .cloned()
        .ok_or_else(|| anyhow!("no resource pack download for device `{device}` or default"))?;
    // These numeric fields cross the frontend's JavaScript Number boundary.
    anyhow::ensure!(
        entry
            .version_code
            .is_none_or(|code| code <= MAX_SAFE_VERSION_CODE),
        "resource pack versionCode exceeds JavaScript's maximum safe integer"
    );
    Ok(entry)
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> ManifestV2 {
        ManifestV2 {
            item: ManifestItemV2 {
                id: "theme.id".into(),
                restype: ResourceTypeV2::ResPack,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn download(code: Option<u64>) -> ManifestDownloadV2 {
        ManifestDownloadV2 {
            version: "1.0".into(),
            file_name: "theme.crp".into(),
            version_code: code,
            url: None,
            sha256: None,
            display_name: None,
            updatelogs: None,
        }
    }

    #[test]
    fn pack_index_requires_exact_id_and_pack_type() {
        let mut index = vec![IndexV2 {
            id: "theme.id".into(),
            name: "Display Name".into(),
            restype: ResourceTypeV2::ResPack,
            repo_owner: String::new(),
            repo_name: String::new(),
            repo_commit_hash: String::new(),
            icon: String::new(),
            cover: String::new(),
            tags: Vec::new(),
            device_vendors: Vec::new(),
            devices: Vec::new(),
            paid_type: PaidTypeV2::Free,
        }];
        assert!(find_pack(&index, "theme.id").is_ok());
        assert!(find_pack(&index, "Display Name").is_err());
        assert!(find_pack(&index, "THEME.ID").is_err());
        index[0].restype = ResourceTypeV2::QuickApp;
        assert!(find_pack(&index, "theme.id").is_err());
    }

    #[test]
    fn pack_manifest_requires_matching_id_and_type() {
        let mut manifest = manifest();
        manifest
            .downloads
            .insert("default".into(), download(Some(2)));
        assert!(select_pack_download(&manifest, "other.id", "device").is_err());
        manifest.item.restype = ResourceTypeV2::WatchFace;
        assert!(select_pack_download(&manifest, "theme.id", "device").is_err());
    }

    #[test]
    fn pack_download_uses_device_then_default_never_arbitrary() {
        let mut manifest = manifest();
        manifest
            .downloads
            .insert("other".into(), download(Some(99)));
        assert!(select_pack_download(&manifest, "theme.id", "device").is_err());
        manifest
            .downloads
            .insert("default".into(), download(Some(2)));
        assert_eq!(
            select_pack_download(&manifest, "theme.id", "device")
                .unwrap()
                .version_code,
            Some(2)
        );
        manifest
            .downloads
            .insert("device".into(), download(Some(3)));
        assert_eq!(
            select_pack_download(&manifest, "theme.id", "device")
                .unwrap()
                .version_code,
            Some(3)
        );
    }

    #[test]
    fn pack_version_code_must_be_a_safe_javascript_integer() {
        let mut manifest = manifest();
        manifest
            .downloads
            .insert("default".into(), download(Some(MAX_SAFE_VERSION_CODE)));
        assert!(select_pack_download(&manifest, "theme.id", "device").is_ok());
        manifest
            .downloads
            .insert("default".into(), download(Some(MAX_SAFE_VERSION_CODE + 1)));
        assert!(select_pack_download(&manifest, "theme.id", "device").is_err());
        // An unsafe device entry must not borrow a safe default's version.
        manifest
            .downloads
            .insert("default".into(), download(Some(2)));
        manifest
            .downloads
            .insert("device".into(), download(Some(u64::MAX)));
        assert!(select_pack_download(&manifest, "theme.id", "device").is_err());
    }

    #[test]
    fn pack_missing_device_version_does_not_borrow_default_version() {
        let mut manifest = manifest();
        manifest.downloads.insert("device".into(), download(None));
        manifest
            .downloads
            .insert("default".into(), download(Some(2)));
        assert_eq!(
            select_pack_download(&manifest, "theme.id", "device")
                .unwrap()
                .version_code,
            None
        );
    }
}
