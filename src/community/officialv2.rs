use std::sync::Arc;

use crate::{
    cdn::GitHubCdn,
    community::{
        CommunityProvider,
        models::{
            common::{
                ManifestItemV2, ManifestV2, ProgressData, ProviderState, SearchConfig, SortRuleV2,
            },
            official::{DeviceMapV2, DeviceV2, IndexV2},
        },
    },
};
use arc_swap::ArcSwap;
use async_trait::async_trait;
use rand::seq::SliceRandom;
use reqwest::Client;

#[derive(Debug)]
pub struct OfficialV2Provider {
    client: Client,
    cdn: GitHubCdn,
    index: ArcSwap<Vec<IndexV2>>,
    splited_index: ArcSwap<Vec<Vec<IndexV2>>>,
    splited_limit: ArcSwap<usize>,
    device_map: ArcSwap<DeviceMapV2>,
    explore: ArcSwap<serde_json::Value>,
    state: ArcSwap<ProviderState>,
}

impl OfficialV2Provider {
    pub fn new(cdn: GitHubCdn) -> Self {
        Self {
            client: crate::net::default_client(),
            cdn,
            index: ArcSwap::new(Arc::new(Vec::new())),
            splited_index: ArcSwap::new(Arc::new(Vec::new())),
            splited_limit: ArcSwap::new(Arc::new(0)),
            device_map: ArcSwap::new(Arc::new(DeviceMapV2::default())),
            explore: ArcSwap::new(Arc::new(serde_json::Value::Null)),
            state: ArcSwap::new(Arc::new(ProviderState::Updating)),
        }
    }

    pub fn device_map(&self) -> Arc<DeviceMapV2> {
        self.device_map.load().clone()
    }

    pub fn device_map_all(&self) -> Vec<DeviceV2> {
        let mut all = (*self.device_map()).clone().xiaomi;
        all.append(&mut (*self.device_map()).clone().vivo);

        all
    }

    pub fn explore(&self) -> Arc<serde_json::Value> {
        self.explore.load().clone()
    }

    pub fn device_map_id_to_name(&self, id: &str) -> Option<String> {
        for dev in self.device_map_all() {
            if dev.id == id {
                return Some(dev.name.clone());
            }
        }
        None
    }

    pub fn device_map_name_to_id(&self, name: &str) -> Option<String> {
        for dev in self.device_map_all() {
            if dev.name == name {
                return Some(dev.id.clone());
            }
        }
        None
    }

    fn split_index(&self, limit: usize, sort: SortRuleV2) {
        let index = self.index.load().clone();
        let mut rng = rand::rng();
        let mut sorted_index = (*index).clone();

        match sort {
            SortRuleV2::Random => sorted_index.shuffle(&mut rng),
            SortRuleV2::Name => {
                sorted_index.sort_by(|a, b| a.name.cmp(&b.name));
            }
            SortRuleV2::Time => {
                sorted_index.reverse();
            }
        };

        let splited_index = sorted_index
            .chunks(limit)
            .map(|c| c.to_vec())
            .collect::<Vec<_>>();
        self.splited_index.store(Arc::new(splited_index));
        self.splited_limit.store(Arc::new(limit));
    }

    pub fn build_repo_raw_url(&self, owner: &str, name: &str, commit_hash: &str) -> String {
        format!(
            "https://raw.githubusercontent.com/{}/{}/{}",
            owner, name, commit_hash
        )
    }

    pub fn build_repo_cdn_url(&self, owner: &str, name: &str, commit_hash: &str) -> String {
        self.cdn
            .convert_url(&self.build_repo_raw_url(owner, name, commit_hash))
    }

    pub fn build_repo_cdn_url_by_index_item(&self, item: &IndexV2) -> String {
        self.build_repo_cdn_url(
            &item.repo_owner.clone(),
            &item.repo_name.clone(),
            &item.repo_commit_hash.clone(),
        )
    }

    pub async fn get_manifest(
        &self,
        owner: &str,
        name: &str,
        commit_hash: &str,
    ) -> anyhow::Result<ManifestV2> {
        let url = format!(
            "{}/manifest_v2.json",
            self.build_repo_cdn_url(owner, name, commit_hash)
        );
        let response = self.client.get(&url).send().await?;
        let text = response.text().await?;
        let manifest: ManifestV2 = serde_json::from_str(&text)?;
        Ok(manifest)
    }
}

#[async_trait]
impl CommunityProvider for OfficialV2Provider {
    fn provider_name(&self) -> String {
        "OfficialV2".to_string()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn state(&self) -> ProviderState {
        let state = self.state.load().clone();
        (*state).clone()
    }

    async fn refresh(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn get_page(
        &self,
        page: u32,
        limit: u32,
        search: SearchConfig,
    ) -> anyhow::Result<Vec<ManifestV2>> {
        if !(*(self.splited_limit.load().clone())).clone() != limit as usize {
            self.split_index(limit as usize, search.sort.clone());
        }

        let splited_index = self.splited_index.load().clone();
        let mut target_page = splited_index[page as usize].clone();

        if let Some(categories) = search.category {
            target_page.retain(|item| {
                item.devices
                    .iter()
                    .any(|category| categories.contains(category))
            });
        }

        if let Some(keyword) = &search.filter {
            target_page.retain(|item| {
                item.name.contains(keyword) || item.tags.iter().any(|tag| tag.contains(keyword))
            });
        }

        let mut ret = Vec::new();
        for item in target_page.iter() {
            ret.push(ManifestV2 {
                item: ManifestItemV2 {
                    id: item.id.clone(),
                    preview: vec![format!(
                        "{}/{}",
                        self.build_repo_cdn_url_by_index_item(&item),
                        item.cover.clone()
                    )],
                    icon: format!(
                        "{}/{}",
                        self.build_repo_cdn_url_by_index_item(&item),
                        item.icon.clone()
                    ),
                    ..Default::default()
                },
                ..Default::default()
            });
        }

        Ok(ret)
    }

    async fn get_categories(&self) -> anyhow::Result<Vec<String>> {
        let mut categories = vec![
            "hidden_paid".to_string(),       // 隐藏付费
            "hidden_force_paid".to_string(), // 隐藏强制付费
            "quickapp".to_string(),          // 快应用
            "watchface".to_string(),         // 表盘
        ];

        let device_map = self.device_map.load();
        device_map.xiaomi.iter().for_each(|xmdev| {
            if !categories.contains(&xmdev.name) {
                categories.push(xmdev.name.clone());
            }
        });

        // TODO: 在支持Vivo设备后也显示vivo设备的分类

        Ok(categories)
    }
    async fn get_item_manifest(&self, item_id: String) -> anyhow::Result<ManifestV2> {
        let index = self.index.load().clone();
        let target_item = index.iter().find(|item| item.id == item_id);

        if let Some(item) = target_item {
            let manifest = self
                .get_manifest(&item.repo_owner, &item.repo_name, &item.repo_commit_hash)
                .await?;
            Ok(ManifestV2 {
                item: ManifestItemV2 {
                    icon: format!(
                        "{}/{}",
                        self.build_repo_cdn_url_by_index_item(&item),
                        item.icon
                    ),
                    preview: manifest
                        .item
                        .preview
                        .iter()
                        .map(|p| format!("{}/{}", self.build_repo_cdn_url_by_index_item(&item), p))
                        .collect::<Vec<_>>(),
                    ..manifest.item
                },
                ..manifest
            })
        } else {
            Err(anyhow::anyhow!("Item not found"))
        }
    }
    async fn download(
        &self,
        item_id: String,
        device: String,
        progress_cb: Option<Box<dyn Fn(ProgressData) + Send>>,
    ) -> anyhow::Result<std::path::PathBuf> {
        Ok(std::path::PathBuf::new())
    }
    async fn get_total_items(&self) -> anyhow::Result<u64> {
        Ok(self.index.load().len() as u64)
    }
}
