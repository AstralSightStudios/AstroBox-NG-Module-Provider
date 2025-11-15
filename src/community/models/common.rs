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
    pub ext: serde_json::Value,
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

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub enum ResourceTypeV2 {
    #[default]
    #[serde(rename = "quick_app")]
    QuickApp, // 快应用
    #[serde(rename = "watchface")]
    WatchFace, // 表盘
    #[serde(rename = "firmware")]
    Firmware, // 固件
    #[serde(rename = "fontpack")]
    FontPack, // 字体包
    #[serde(rename = "iconpack")]
    IconPack, // 图标包
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum PaidTypeV2 {
    #[serde(rename = "")]
    Free, // 免费
    #[serde(rename = "paid")]
    Paid, // 付费（内含付费内容）
    #[serde(rename = "force_paid")]
    ForcePaid, // 强制付费（不给钱不让用）
}
