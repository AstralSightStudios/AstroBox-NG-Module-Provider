use std::{
    cmp,
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

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
use anyhow::{Context, anyhow};
use arc_swap::ArcSwap;
use async_trait::async_trait;
use futures_util::StreamExt;
use rand::seq::SliceRandom;
use reqwest::Client;
use tauri::{AppHandle, Manager};
use tokio::{
    fs::{self, File},
    io::AsyncWriteExt,
};

pub struct OfficialV2Provider {
    client: Client,
    cdn: ArcSwap<GitHubCdn>,
    app_handle: AppHandle,
    index: ArcSwap<Vec<IndexV2>>,
    splited_index: ArcSwap<Vec<Vec<IndexV2>>>,
    splited_limit: ArcSwap<usize>,
    device_map: ArcSwap<DeviceMapV2>,
    explore: ArcSwap<serde_json::Value>,
    state: ArcSwap<ProviderState>,
}

impl OfficialV2Provider {
    pub fn new(cdn: GitHubCdn, app_handle: AppHandle) -> Self {
        Self {
            client: crate::net::default_client(),
            cdn: ArcSwap::new(Arc::new(cdn)),
            app_handle,
            index: ArcSwap::new(Arc::new(Vec::new())),
            splited_index: ArcSwap::new(Arc::new(Vec::new())),
            splited_limit: ArcSwap::new(Arc::new(0)),
            device_map: ArcSwap::new(Arc::new(DeviceMapV2::default())),
            explore: ArcSwap::new(Arc::new(serde_json::Value::Null)),
            state: ArcSwap::new(Arc::new(ProviderState::Updating)),
        }
    }

    pub fn set_cdn(&self, cdn: GitHubCdn) {
        self.cdn.store(Arc::new(cdn));
    }

    fn cache_root(&self) -> anyhow::Result<PathBuf> {
        let base = self
            .app_handle
            .path()
            .app_cache_dir()
            .map_err(|err| anyhow!("app cache directory unavailable: {err}"))?;
        Ok(base.join("community").join("official_v2"))
    }

    pub fn device_map(&self) -> Arc<DeviceMapV2> {
        self.device_map.load().clone()
    }

    pub fn device_map_all(&self) -> Vec<DeviceV2> {
        let mut all: Vec<DeviceV2> = (*self.device_map())
            .clone()
            .xiaomi
            .values()
            .cloned()
            .collect();
        all.append(
            &mut (*self.device_map())
                .clone()
                .vivo
                .values()
                .cloned()
                .collect(),
        );

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
        let cdn = *self.cdn.load_full();
        cdn.convert_url(&self.build_repo_raw_url(owner, name, commit_hash))
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
    fn state(&self) -> ProviderState {
        let state = self.state.load().clone();
        (*state).clone()
    }

    async fn refresh(&self, cfg: &str) -> anyhow::Result<()> {
        self.state.store(Arc::new(ProviderState::Updating));

        //更新cdn

        let cfg: HashMap<String, _> = serde_json::from_str(cfg).unwrap_or(HashMap::new());
        let cdn: GitHubCdn = *cfg.get("cdn").unwrap_or(&GitHubCdn::Raw);
        self.cdn.store(Arc::new(cdn));

        // 更新index
        let url = (*self.cdn.load_full()).convert_url("https://raw.githubusercontent.com/AstralSightStudios/AstroBox-Repo/refs/heads/main/index_v2.csv");
        let resp = self.client.get(&url).send().await?.error_for_status()?;
        let raw = resp.bytes().await?;
        let mut list: Vec<IndexV2> = Vec::new();
        let mut csv_read = csv::Reader::from_reader(raw.as_ref());
        for it in csv_read.deserialize() {
            list.push(it?);
        }
        self.index.store(Arc::new(list));
        self.split_index(114514, SortRuleV2::Random);

        // 更新设备map
        let url = (*self.cdn.load_full()).convert_url("https://raw.githubusercontent.com/AstralSightStudios/AstroBox-Repo/refs/heads/main/devices_v2.json");
        let resp = self.client.get(&url).send().await?.error_for_status()?;
        let map: DeviceMapV2 = resp.json().await?;
        self.device_map.store(Arc::new(map));

        // 更新探索页
        let url = (*self.cdn.load_full()).convert_url("https://raw.githubusercontent.com/AstralSightStudios/AstroBox-Repo/refs/heads/main/explore_v2.json");
        let resp = self.client.get(&url).send().await?.error_for_status()?;
        let explore: serde_json::Value = resp.json().await?;
        self.explore.store(Arc::new(explore));

        self.state.store(Arc::new(ProviderState::Ready));

        Ok(())
    }

    async fn get_page(
        &self,
        page: u32,
        limit: u32,
        search: SearchConfig,
    ) -> anyhow::Result<Vec<ManifestItemV2>> {
        if !(*(self.splited_limit.load().clone())) != limit as usize {
            self.split_index(limit as usize, search.sort.clone());
        }

        if self.splited_index.load().len() <= page as usize {
            return Ok(Vec::new());
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
            ret.push(ManifestItemV2 {
                id: item.id.clone(),
                name: item.name.clone(),
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
                cover: format!(
                    "{}/{}",
                    self.build_repo_cdn_url_by_index_item(&item),
                    item.cover.clone()
                ),
                restype: item.restype.clone(),

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
        device_map
            .xiaomi
            .values()
            .collect::<Vec<_>>()
            .iter()
            .for_each(|xmdev| {
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
        let index = self.index.load();
        let index_ref = index.clone();
        let item = index_ref
            .iter()
            .find(|entry| entry.id == item_id)
            .cloned()
            .ok_or_else(|| anyhow!("Item not found"))?;

        let manifest = self
            .get_manifest(&item.repo_owner, &item.repo_name, &item.repo_commit_hash)
            .await
            .with_context(|| format!("failed to fetch manifest for {}", item.name))?;

        let downloads = &manifest.downloads;
        let download_entry = downloads
            .get(&device)
            .or_else(|| downloads.get("default"))
            .or_else(|| downloads.values().next())
            .cloned()
            .ok_or_else(|| anyhow!("no downloadable artifact for device `{device}`"))?;

        let mut file_name = download_entry.file_name.trim().to_string();
        if file_name.is_empty() {
            if let Some(url) = &download_entry.url {
                if let Some(name) = url.split('/').last() {
                    file_name = name.to_string();
                }
            }
        }
        if file_name.is_empty() {
            return Err(anyhow!("download entry missing file name"));
        }

        let resolved_url = if let Some(url) = &download_entry.url {
            (*self.cdn.load_full()).convert_url(url)
        } else {
            format!(
                "{}/{}",
                self.build_repo_cdn_url_by_index_item(&item),
                &file_name
            )
        };

        let cache_root = self.cache_root()?;
        let item_dir = cache_root.join(&item.id);
        fs::create_dir_all(&item_dir)
            .await
            .with_context(|| format!("failed to create cache directory {}", item_dir.display()))?;

        let final_path = item_dir.join(&file_name);
        let unique_suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let tmp_path = item_dir.join(format!("{}.{}.part", unique_suffix, file_name));
        let client = self.client.clone();
        let cleanup_path = tmp_path.clone();
        let download_result = {
            let resolved_url = resolved_url;
            let final_path = final_path;
            let tmp_path = tmp_path;
            let progress_cb = progress_cb;
            async move {
                let mut file = File::create(&tmp_path).await.with_context(|| {
                    format!("failed to create temp file {}", tmp_path.display())
                })?;

                if let Some(cb) = progress_cb.as_ref() {
                    cb(ProgressData {
                        progress: 0.0,
                        status: "downloading".into(),
                    });
                }

                let response = client
                    .get(&resolved_url)
                    .send()
                    .await
                    .with_context(|| format!("failed to request {}", resolved_url))?
                    .error_for_status()
                    .with_context(|| {
                        format!("download request returned error for {}", resolved_url)
                    })?;

                let total = response.content_length();
                let mut stream = response.bytes_stream();
                let mut downloaded: u64 = 0;
                let mut last_emit = Instant::now();
                let step_bytes = total.map(|t| cmp::max(1, t / 100));
                let mut last_reported = 0u64;

                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.with_context(|| "failed to read download chunk")?;
                    downloaded += chunk.len() as u64;
                    file.write_all(chunk.as_ref())
                        .await
                        .with_context(|| "failed to write download chunk")?;

                    if let Some(cb) = progress_cb.as_ref() {
                        let mut emit = last_emit.elapsed() >= Duration::from_millis(200);
                        if !emit {
                            if let Some(step) = step_bytes {
                                if downloaded >= last_reported.saturating_add(step)
                                    || total.map(|t| downloaded >= t).unwrap_or(false)
                                {
                                    emit = true;
                                }
                            }
                        }

                        if emit {
                            let progress = match total {
                                Some(total_len) if total_len > 0 => {
                                    (downloaded as f32 / total_len as f32).clamp(0.0, 1.0)
                                }
                                _ => 0.0,
                            };
                            cb(ProgressData {
                                progress,
                                status: "downloading".into(),
                            });
                            last_emit = Instant::now();
                            if step_bytes.is_some() {
                                last_reported = downloaded;
                            }
                        }
                    }
                }

                file.flush()
                    .await
                    .with_context(|| format!("failed to flush {}", tmp_path.display()))?;

                drop(file);

                fs::rename(&tmp_path, &final_path).await.with_context(|| {
                    format!(
                        "failed to move downloaded file {} -> {}",
                        tmp_path.display(),
                        final_path.display()
                    )
                })?;

                if let Some(cb) = progress_cb.as_ref() {
                    cb(ProgressData {
                        progress: 1.0,
                        status: "finished".into(),
                    });
                }

                Ok::<_, anyhow::Error>(final_path.clone())
            }
        }
        .await;

        if download_result.is_err() {
            let _ = fs::remove_file(&cleanup_path).await;
        }

        download_result
    }
    async fn get_total_items(&self) -> anyhow::Result<u64> {
        Ok(self.index.load().len() as u64)
    }
}
