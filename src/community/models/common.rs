use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProgressData {
    pub progress: f32,
    pub status: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum ProviderState {
    Ready,
    Updating,
    Failed(String),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SearchConfig {
    pub filter: Option<String>,
    pub sort: SortRuleV2,
    pub category: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SortRuleV2 {
    Random,
    Name,
    Time,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct ManifestV2 {
    pub item: ManifestItemV2,
    pub links: Vec<ManifestLinkV2>,
    pub downloads: HashMap<String, ManifestDownloadV2>,
    /// Provider-specific extensions. `bundledResources` is parsed by
    /// [`ManifestV2::bundled_resources`]; retaining the raw value keeps all
    /// existing and future extension fields forward-compatible.
    pub ext: serde_json::Value,
}

impl ManifestV2 {
    pub fn bundled_resources(&self) -> Option<ManifestBundledResourcesV2> {
        self.ext
            .get("bundledResources")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
    }
}

/// Optional `manifest_v2.json` extension at `ext.bundledResources`.
///
/// Resource entries refer to any resource ID exposed by a community provider;
/// plugin entries refer to a plugin marketplace manifest name. A resource entry
/// without `provider` inherits the provider of the declaring manifest.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestBundledResourcesV2 {
    #[serde(default)]
    pub required: Vec<ManifestBundledResourceV2>,
    #[serde(default)]
    pub recommended: Vec<ManifestBundledResourceV2>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ManifestBundledResourceV2 {
    #[serde(rename = "type")]
    pub resource_type: ManifestBundledResourceTypeV2,
    pub id: String,
    #[serde(default)]
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ManifestBundledResourceTypeV2 {
    Resource,
    Plugin,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct ManifestItemV2 {
    pub id: String,
    pub restype: ResourceTypeV2,
    pub name: String,
    pub description: String,
    pub preview: Vec<String>,
    pub icon: String,
    pub cover: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paid_type: Option<PaidTypeV2>,
    pub author: Vec<ManifestAuthorV2>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ManifestAuthorV2 {
    pub name: String,
    #[serde(default)]
    #[serde(rename = "bindABAccount")]
    pub bind_ab_account: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ManifestLinkV2 {
    #[serde(default)]
    pub icon: Option<String>,
    pub title: String,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ManifestDownloadV2 {
    pub version: String,
    pub file_name: String,
    #[serde(default, rename = "versionCode", alias = "version_code")]
    pub version_code: Option<u64>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub updatelogs: Option<Vec<ManifestDownloadUpdateLogV2>>,
    /// What a native `.canopus` download needs from the band. Absent on every
    /// other resource type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canopus: Option<ManifestCanopusV2>,
}

/// Compatibility of a native Canopus download (refactor-plan P5.7). A portable
/// module lists no targets; a target-bound one lists every exact target id its
/// package carries an artifact for.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct ManifestCanopusV2 {
    /// Module ABI major the package was built against.
    pub abi: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
    /// Lowest Supervisor framework revision the module needs.
    #[serde(default, rename = "minFramework", skip_serializing_if = "Option::is_none")]
    pub min_framework: Option<u32>,
}

/// What the connected band reports about itself.
#[derive(Debug, Clone, PartialEq)]
pub struct CanopusDeviceCompat<'a> {
    /// `None` on a Supervisor too old to report it: a target is never guessed.
    pub target_id: Option<&'a str>,
    pub framework_revision: u32,
    /// The module ABI major this client speaks.
    pub abi_major: u32,
}

impl ManifestCanopusV2 {
    /// Err names the first reason the download cannot run on this band.
    pub fn check(&self, device: &CanopusDeviceCompat<'_>) -> Result<(), String> {
        if self.abi != device.abi_major {
            return Err(format!(
                "built for Canopus ABI {}, this band speaks {}",
                self.abi, device.abi_major
            ));
        }
        if let Some(min) = self.min_framework {
            if device.framework_revision < min {
                return Err(format!(
                    "needs Canopus framework revision {min}, the band has {}",
                    device.framework_revision
                ));
            }
        }
        if !self.targets.is_empty() {
            match device.target_id {
                None => return Err("the band does not report its firmware target".into()),
                Some(id) if !self.targets.iter().any(|t| t == id) => {
                    return Err(format!("no build for firmware target {id}"));
                }
                Some(_) => {}
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ManifestDownloadUpdateLogV2 {
    pub version: String,
    pub content: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub enum ResourceTypeV2 {
    #[default]
    #[serde(rename = "quick_app")]
    QuickApp, // 快应用
    #[serde(rename = "watchface")]
    WatchFace, // 表盘
    #[serde(rename = "canopus")]
    Canopus, // 模块
    #[serde(rename = "firmware")]
    Firmware, // 固件
    #[serde(rename = "res_pack")]
    ResPack, // 资源包
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub enum PaidTypeV2 {
    #[serde(rename = "")]
    Free, // 免费
    #[serde(rename = "paid")]
    Paid, // 付费（内含付费内容）
    #[serde(rename = "force_paid")]
    ForcePaid, // 强制付费（不给钱不让用）
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bundled_resources_extension_without_consuming_other_extensions() {
        let manifest: ManifestV2 = serde_json::from_value(serde_json::json!({
            "item": {
                "id": "main-resource",
                "restype": "watchface",
                "name": "Main resource",
                "description": "",
                "preview": [],
                "icon": "",
                "cover": "",
                "author": []
            },
            "links": [],
            "downloads": {},
            "ext": {
                "bundledResources": {
                    "required": [
                        { "type": "resource", "id": "base-module" }
                    ],
                    "recommended": [
                        {
                            "type": "plugin",
                            "id": "watchface-tools",
                            "provider": "ignored-for-plugins"
                        }
                    ]
                },
                "anotherFutureExtension": true
            }
        }))
        .expect("manifest should deserialize");

        let bundled = manifest.bundled_resources().expect("bundle should parse");
        assert_eq!(bundled.required.len(), 1);
        assert_eq!(bundled.required[0].id, "base-module");
        assert_eq!(bundled.recommended.len(), 1);
        assert_eq!(
            bundled.recommended[0].resource_type,
            ManifestBundledResourceTypeV2::Plugin
        );
        assert!(manifest.ext.get("anotherFutureExtension").is_some());
    }
    fn band(target: Option<&'static str>, framework: u32) -> CanopusDeviceCompat<'static> {
        CanopusDeviceCompat { target_id: target, framework_revision: framework, abi_major: 1 }
    }

    #[test]
    fn canopus_download_names_what_it_needs_from_the_band() {
        let download: ManifestDownloadV2 = serde_json::from_value(serde_json::json!({
            "version": "1.2.0",
            "file_name": "demo-1.2.0.canopus",
            "versionCode": 12,
            "canopus": { "abi": 1, "targets": ["xiaomi-band-11-4.100.155"], "minFramework": 3 }
        }))
        .expect("download should deserialize");
        let needs = download.canopus.expect("canopus metadata");
        assert_eq!(needs.targets, vec!["xiaomi-band-11-4.100.155"]);
        assert_eq!(needs.min_framework, Some(3));

        assert!(needs.check(&band(Some("xiaomi-band-11-4.100.155"), 3)).is_ok());
        assert!(needs.check(&band(Some("xiaomi-band-11-4.100.139"), 3)).unwrap_err().contains("4.100.139"));
        assert!(needs.check(&band(None, 3)).unwrap_err().contains("does not report"));
        assert!(needs.check(&band(Some("xiaomi-band-11-4.100.155"), 2)).unwrap_err().contains("framework"));
        let other_abi = CanopusDeviceCompat { abi_major: 2, ..band(Some("xiaomi-band-11-4.100.155"), 3) };
        assert!(needs.check(&other_abi).unwrap_err().contains("ABI"));
    }

    #[test]
    fn portable_canopus_download_lists_no_targets() {
        let needs: ManifestCanopusV2 =
            serde_json::from_value(serde_json::json!({ "abi": 1 })).expect("abi only");
        assert!(needs.targets.is_empty() && needs.min_framework.is_none());
        assert!(needs.check(&band(None, 0)).is_ok());
        let again = serde_json::to_value(&needs).unwrap();
        assert_eq!(again, serde_json::json!({ "abi": 1 }));
    }
}
