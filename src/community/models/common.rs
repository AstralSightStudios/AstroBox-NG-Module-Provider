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
    #[serde(rename = "fontpack")]
    FontPack, // 字体包
    #[serde(rename = "iconpack")]
    IconPack, // 图标包
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
}
