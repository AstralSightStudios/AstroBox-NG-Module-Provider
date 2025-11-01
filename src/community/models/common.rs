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
    pub downloads: Vec<ManifestDownloadV2>,
    pub ext: serde_json::Value,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct ManifestItemV2 {
    pub id: String,
    pub name: String,
    pub description: String,
    pub preview: Vec<String>,
    pub icon: String,
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
    pub icon: String,
    pub title: String,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ManifestDownloadV2 {
    pub version: String,
    pub filename: String,
    pub updatelogs: Vec<ManifestUpdateLogV2>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ManifestUpdateLogV2 {
    pub version: String,
    pub content: String,
}
