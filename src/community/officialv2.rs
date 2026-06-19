use std::{
    cmp,
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::{
    cdn::GitHubCdn,
    community::{
        CommunityProvider,
        models::{
            common::{
                ManifestDownloadV2, ManifestItemV2, ManifestV2, PaidTypeV2, ProgressData,
                ProviderState, ResourceTypeV2, SearchConfig, SortRuleV2,
            },
            official::{DeviceMapV2, DeviceV2, IndexV2},
        },
    },
};
use account::AccountStore;
use anyhow::{Context, anyhow};
use arc_swap::ArcSwap;
use base64::Engine as _;
use async_trait::async_trait;
use futures_util::StreamExt;
use rand::seq::SliceRandom;
use regex::Regex;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tokio::{
    fs::{self, File},
    io::AsyncWriteExt,
};

const HIDE_PAID: &str = "hide_paid"; // 隐藏付费
const HIDE_FORCE_PAID: &str = "hide_force_paid"; // 隐藏强制付费
const QUICK_APP: &str = "quick_app"; // 快应用
const WATCHFACE: &str = "watchface"; // 表盘
const ACCOUNT_SOURCE_STORAGE_KEY: &str = "network_account_source_cfg";
const ASTROBOX_ACCOUNT_PROVIDER: &str = "astrobox";

// 选中官方镜像源时，图片经境内 CDN 取回后内联为 base64 data URI（绕开 webview 直连 GitHub）
const MAX_INLINE_IMAGE_BYTES: usize = 4 * 1024 * 1024; // 单张内联上限，超过则回退原始 URL
const IMAGE_B64_CACHE_CAP: usize = 1024; // 内存缓存条数上限；内容按 commit 寻址、不可变
const IMAGE_INLINE_CONCURRENCY: usize = 12; // 单页内联的并发抓取数

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountSourceConfig {
    source: Option<AccountSourceId>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
enum AccountSourceId {
    CasAstralsight,
    WaterFlames,
}

impl Default for AccountSourceId {
    fn default() -> Self {
        Self::CasAstralsight
    }
}

impl AccountSourceId {
    fn astrobox_api_base_url(self) -> &'static str {
        match self {
            Self::CasAstralsight => "https://astrobox-api.astralsight.space",
            Self::WaterFlames => "https://asastrobox-api.waterflames.cn",
        }
    }
}

#[derive(Debug, Serialize)]
struct SourceCdnDownloadRequest {
    id: String,
    device: Option<String>,
    node: &'static str,
}

#[derive(Debug, Deserialize)]
struct SourceCdnDownloadResponse {
    url: String,
    accelerated: bool,
    #[allow(dead_code)]
    #[serde(rename = "expiresAt")]
    expires_at: Option<u64>,
    #[allow(dead_code)]
    node: Option<String>,
}

#[derive(Debug, Serialize)]
struct SourceCdnImagesItem {
    id: String,
    paths: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SourceCdnImagesRequest {
    items: Vec<SourceCdnImagesItem>,
    node: &'static str,
}

#[derive(Debug, Deserialize)]
struct SourceCdnImageEntry {
    path: String,
    url: String,
    accelerated: bool,
}

#[derive(Debug, Deserialize)]
struct SourceCdnImagesResultItem {
    id: String,
    images: Vec<SourceCdnImageEntry>,
}

#[derive(Debug, Deserialize)]
struct SourceCdnImagesResponse {
    results: Vec<SourceCdnImagesResultItem>,
}

// 一次图片内联请求：定位某资源仓内某相对图片
struct ImageRef {
    id: String,
    owner: String,
    repo: String,
    commit: String,
    rel: String, // 规范化的仓内相对路径(无前导 /)
}

// content-type 缺失时按 URL 扩展名兜底推断图片 MIME
fn guess_image_mime(url: &str) -> &'static str {
    let path = url.split(['?', '#']).next().unwrap_or(url).to_ascii_lowercase();
    if path.ends_with(".png") {
        "image/png"
    } else if path.ends_with(".jpg") || path.ends_with(".jpeg") {
        "image/jpeg"
    } else if path.ends_with(".webp") {
        "image/webp"
    } else if path.ends_with(".gif") {
        "image/gif"
    } else if path.ends_with(".svg") {
        "image/svg+xml"
    } else if path.ends_with(".bmp") {
        "image/bmp"
    } else if path.ends_with(".avif") {
        "image/avif"
    } else {
        "image/png"
    }
}

pub struct OfficialV2Provider {
    cdn: ArcSwap<GitHubCdn>,
    app_handle: AppHandle,
    index: ArcSwap<Vec<IndexV2>>,
    splited_index: ArcSwap<Vec<Vec<IndexV2>>>,
    splited_limit: ArcSwap<usize>,
    device_map: ArcSwap<DeviceMapV2>,
    explore: ArcSwap<serde_json::Value>,
    state: ArcSwap<ProviderState>,
    placeholder_index: ArcSwap<u32>,
    // 图片 base64 内联缓存：cosKey -> data URI（commit 寻址、不可变）
    image_b64_cache: Mutex<HashMap<String, Arc<str>>>,
}

impl OfficialV2Provider {
    pub fn new(cdn: GitHubCdn, app_handle: AppHandle) -> Self {
        Self {
            cdn: ArcSwap::new(Arc::new(cdn)),
            app_handle,
            index: ArcSwap::new(Arc::new(Vec::new())),
            splited_index: ArcSwap::new(Arc::new(Vec::new())),
            splited_limit: ArcSwap::new(Arc::new(0)),
            device_map: ArcSwap::new(Arc::new(DeviceMapV2::default())),
            explore: ArcSwap::new(Arc::new(serde_json::Value::Null)),
            state: ArcSwap::new(Arc::new(ProviderState::Updating)),
            placeholder_index: ArcSwap::new(Arc::new(0)),
            image_b64_cache: Mutex::new(HashMap::new()),
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

    pub fn device_map_model_to_id(&self, model: &str) -> Option<String> {
        let device_map = self.device_map.load();
        if let Some(device) = device_map.xiaomi.get(model) {
            return Some(device.id.clone());
        }
        if let Some(device) = device_map.vivo.get(model) {
            return Some(device.id.clone());
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

    fn resolve_repo_asset_url(&self, base: &str, path: &str) -> String {
        if path.starts_with("http://")
            || path.starts_with("https://")
            || path.starts_with("data:")
            || path.starts_with("blob:")
            || path.starts_with("tauri:")
            || path.starts_with('/')
        {
            return path.to_string();
        }
        format!(
            "{}/{}",
            base.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    async fn current_account_source(&self) -> AccountSourceId {
        account::local_storage_get_json::<AccountSourceConfig>(
            &self.app_handle,
            ACCOUNT_SOURCE_STORAGE_KEY,
        )
        .await
        .ok()
        .flatten()
        .and_then(|cfg| cfg.source)
        .unwrap_or_default()
    }

    async fn current_astrobox_token(&self) -> anyhow::Result<String> {
        let account = AccountStore::new(ASTROBOX_ACCOUNT_PROVIDER)
            .load(&self.app_handle)
            .await
            .context("failed to read AstroBox account")?
            .ok_or_else(|| anyhow!("请先登录 AstroBox 账号"))?;
        account
            .token
            .filter(|token| !token.trim().is_empty())
            .ok_or_else(|| anyhow!("请先登录 AstroBox 账号"))
    }

    async fn resolve_source_cdn_download_url(
        &self,
        item_id: &str,
        device: Option<&str>,
    ) -> anyhow::Result<String> {
        let token = self.current_astrobox_token().await?;
        let base_url = self.current_account_source().await.astrobox_api_base_url();
        let request = SourceCdnDownloadRequest {
            id: item_id.to_string(),
            device: device
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            node: "edgeone",
        };
        let response = crate::net::default_client()
            .post(format!("{base_url}/source-cdn/download"))
            .header("X-ASTROBOX-TOKEN", token)
            .json(&request)
            .send()
            .await
            .context("failed to request official CDN download URL")?;
        let status = response.status();

        if status == StatusCode::FORBIDDEN {
            return Err(anyhow!("官方加速源需要 AstroBox Pro"));
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(anyhow!("今日官方加速源流量已用完"));
        }
        if status == StatusCode::NOT_FOUND {
            return Err(anyhow!("官方加速源未找到此资源"));
        }

        let response = response
            .error_for_status()
            .context("official CDN download URL request failed")?
            .json::<SourceCdnDownloadResponse>()
            .await
            .context("failed to parse official CDN download URL")?;
        if !response.accelerated {
            log::info!("[OfficialV2] source CDN fallback to GitHub for {}", item_id);
        }
        Ok(response.url)
    }

    // 与服务端 buildCosKey 一致：official-source/{owner}/{repo}/{commit}/{path}
    fn image_cos_key(owner: &str, repo: &str, commit: &str, rel: &str) -> String {
        format!(
            "official-source/{}/{}/{}/{}",
            owner,
            repo,
            commit,
            rel.trim_start_matches('/')
        )
    }

    // 仅相对(同仓)路径可镜像/内联；绝对/外链/data 等返回 None 由调用方按原样处理
    fn relative_image_path(path: &str) -> Option<String> {
        let p = path.trim();
        if p.is_empty()
            || p.starts_with("http://")
            || p.starts_with("https://")
            || p.starts_with("data:")
            || p.starts_with("blob:")
            || p.starts_with("tauri:")
            || p.starts_with('/')
        {
            return None;
        }
        Some(p.trim_start_matches('/').to_string())
    }

    fn image_cache_get(&self, key: &str) -> Option<Arc<str>> {
        self.image_b64_cache.lock().ok()?.get(key).cloned()
    }

    fn image_cache_put(&self, key: &str, value: &str) {
        if let Ok(mut map) = self.image_b64_cache.lock() {
            // 内容不可变，溢出整清即可（无需 LRU）
            if map.len() >= IMAGE_B64_CACHE_CAP {
                map.clear();
            }
            map.insert(key.to_string(), Arc::from(value));
        }
    }

    // 抓取图片并编码为 data URI。优先用响应 content-type，否则按扩展名推断。
    async fn fetch_image_data_uri(url: &str) -> anyhow::Result<String> {
        let resp = crate::net::default_client()
            .get(url)
            .send()
            .await?
            .error_for_status()?;
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let bytes = resp.bytes().await?;
        if bytes.len() > MAX_INLINE_IMAGE_BYTES {
            return Err(anyhow!("image too large to inline: {} bytes", bytes.len()));
        }
        let mime = content_type
            .filter(|c| c.starts_with("image/"))
            .unwrap_or_else(|| guess_image_mime(url).to_string());
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Ok(format!("data:{};base64,{}", mime, b64))
    }

    // 向服务端批量换取图片签名直链（每翻页一次），按资源 id 分组
    async fn resolve_source_cdn_image_urls(
        &self,
        items: HashMap<String, Vec<String>>,
    ) -> anyhow::Result<Vec<(String, Vec<SourceCdnImageEntry>)>> {
        let token = self.current_astrobox_token().await?;
        let base_url = self.current_account_source().await.astrobox_api_base_url();
        let request = SourceCdnImagesRequest {
            items: items
                .into_iter()
                .map(|(id, paths)| SourceCdnImagesItem { id, paths })
                .collect(),
            node: "edgeone",
        };
        let response = crate::net::default_client()
            .post(format!("{base_url}/source-cdn/images"))
            .header("X-ASTROBOX-TOKEN", token)
            .json(&request)
            .send()
            .await
            .context("failed to request official CDN image URLs")?;
        let status = response.status();
        if status == StatusCode::FORBIDDEN {
            return Err(anyhow!("官方加速源需要 AstroBox Pro"));
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(anyhow!("图片加速请求过于频繁"));
        }
        let parsed = response
            .error_for_status()
            .context("official CDN image URL request failed")?
            .json::<SourceCdnImagesResponse>()
            .await
            .context("failed to parse official CDN image URLs")?;
        Ok(parsed
            .results
            .into_iter()
            .map(|r| (r.id, r.images))
            .collect())
    }

    // 把一组图片换成 base64 data URI，返回 cosKey -> data URI。
    // 任一步失败/非 Pro/未镜像，相应图片不入表，调用方回退原始 URL（webview 直连）。
    async fn inline_images(&self, refs: Vec<ImageRef>) -> HashMap<String, String> {
        let mut out: HashMap<String, String> = HashMap::new();
        if refs.is_empty() {
            return out;
        }

        // 按 cosKey 去重
        let mut by_key: HashMap<String, ImageRef> = HashMap::new();
        for r in refs {
            let key = Self::image_cos_key(&r.owner, &r.repo, &r.commit, &r.rel);
            by_key.entry(key).or_insert(r);
        }

        // 先吃缓存，剩下的按 id 分组去签发
        let mut items: HashMap<String, Vec<String>> = HashMap::new();
        let mut coords: HashMap<String, (String, String, String)> = HashMap::new();
        for (key, r) in by_key {
            if let Some(v) = self.image_cache_get(&key) {
                out.insert(key, v.to_string());
                continue;
            }
            items.entry(r.id.clone()).or_default().push(r.rel.clone());
            coords
                .entry(r.id.clone())
                .or_insert((r.owner, r.repo, r.commit));
        }
        if items.is_empty() {
            return out;
        }

        let signed = match self.resolve_source_cdn_image_urls(items).await {
            Ok(s) => s,
            Err(err) => {
                log::warn!("[OfficialV2] image sign failed: {err}");
                return out; // 仅返回缓存命中，其余回退原始 URL
            }
        };

        // 只内联加速直链；非加速(GitHub 兜底)留给调用方用原始 URL
        let mut tasks = Vec::new();
        for (id, entries) in signed {
            let Some((owner, repo, commit)) = coords.get(&id).cloned() else {
                continue;
            };
            for entry in entries {
                if !entry.accelerated {
                    continue;
                }
                let key =
                    Self::image_cos_key(&owner, &repo, &commit, entry.path.trim_start_matches('/'));
                let url = entry.url;
                tasks.push(async move {
                    match Self::fetch_image_data_uri(&url).await {
                        Ok(data) => Some((key, data)),
                        Err(err) => {
                            log::warn!("[OfficialV2] inline image failed {key}: {err}");
                            None
                        }
                    }
                });
            }
        }

        let results: Vec<Option<(String, String)>> = futures_util::stream::iter(tasks)
            .buffer_unordered(IMAGE_INLINE_CONCURRENCY)
            .collect()
            .await;
        for (key, data) in results.into_iter().flatten() {
            self.image_cache_put(&key, &data);
            out.insert(key, data);
        }
        out
    }

    pub async fn get_blog_markdown(&self, path: &str) -> anyhow::Result<String> {
        let cdn = *self.cdn.load_full();
        let raw_url = format!(
            "https://raw.githubusercontent.com/AstralSightStudios/AstroBox-Repo/refs/heads/main/blogs/{}",
            path
        );
        let url = cdn.convert_url(&raw_url);
        let client = crate::net::default_client();
        let resp = client
            .get(&url)
            .send()
            .await?
            .error_for_status()
            .with_context(|| format!("failed to fetch blog markdown from {}", url))?;
        let text = resp.text().await?;

        // Replace naked raw.githubusercontent.com URLs
        let raw_re = Regex::new(
            r#"https://raw\.githubusercontent\.com/AstralSightStudios/AstroBox-Repo/[^)\s\"']+"#,
        )
        .unwrap();
        let text = raw_re.replace_all(&text, |caps: &regex::Captures<'_>| {
            let matched = caps.get(0).unwrap().as_str();
            cdn.convert_url(matched)
        });

        // Resolve relative paths in markdown links/images
        let base_dir = std::path::Path::new(path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        if base_dir.is_empty() {
            return Ok(text.into_owned());
        }

        let rel_re = Regex::new(r"(!?\[[^\]]*\])\(([^)\s]+)\)").unwrap();
        let base_raw = format!(
            "https://raw.githubusercontent.com/AstralSightStudios/AstroBox-Repo/refs/heads/main/blogs/{}/",
            base_dir.trim_end_matches('/')
        );
        let text = rel_re.replace_all(&text, |caps: &regex::Captures<'_>| {
            let prefix = caps.get(1).unwrap().as_str();
            let link = caps.get(2).unwrap().as_str();
            if link.starts_with("http://")
                || link.starts_with("https://")
                || link.starts_with("data:")
                || link.starts_with('/')
            {
                format!("{}({})", prefix, link)
            } else {
                let resolved = format!("{}{}", base_raw, link.trim_start_matches('/'));
                format!("{}({})", prefix, cdn.convert_url(&resolved))
            }
        });

        Ok(text.into_owned())
    }

    pub async fn get_manifest(
        &self,
        owner: &str,
        name: &str,
        commit_hash: &str,
    ) -> anyhow::Result<ManifestV2> {
        let base = self.build_repo_cdn_url(owner, name, commit_hash);
        let client = crate::net::default_client();

        let url_v2 = format!("{}/manifest_v2.json", base);
        let resp_v2 = client.get(&url_v2).send().await?;

        if resp_v2.status() == reqwest::StatusCode::NOT_FOUND {
            // fallback v1 manifest
            let url_v1 = format!("{}/manifest.json", base);
            let resp_v1 = client
                .get(&url_v1)
                .send()
                .await?
                .error_for_status()
                .with_context(|| format!("failed to request legacy manifest `{url_v1}`"))?;

            let text_v1 = resp_v1.text().await?;
            let raw_v1: serde_json::Value = serde_json::from_str(&text_v1)
                .with_context(|| "failed to parse legacy manifest json")?;

            let manifest_v2 = super::legacyparse::manifest_v1_to_v2(raw_v1)
                .with_context(|| "failed to convert legacy manifest v1 -> v2")?;

            Ok(manifest_v2)
        } else {
            let resp_v2 = resp_v2
                .error_for_status()
                .with_context(|| format!("failed to request manifest v2 `{url_v2}`"))?;
            let text_v2 = resp_v2.text().await?;
            let manifest: ManifestV2 = serde_json::from_str(&text_v2)?;
            Ok(manifest)
        }
    }

    pub async fn resolve_download_entry(
        &self,
        item_id: String,
        device: String,
        trial: bool,
    ) -> anyhow::Result<ManifestDownloadV2> {
        let index = self.index.load();
        let index_ref = index.clone();

        let item = index_ref
            .iter()
            .find(|entry| entry.id == item_id)
            .or_else(|| index_ref.iter().find(|entry| entry.name == item_id))
            .cloned()
            .ok_or_else(|| anyhow!("Item not found by id or name"))?;

        let manifest = self
            .get_manifest(&item.repo_owner, &item.repo_name, &item.repo_commit_hash)
            .await
            .with_context(|| format!("failed to fetch manifest for {}", item.name))?;

        let entries = if trial {
            manifest
                .ext
                .get("trialDownloads")
                .cloned()
                .map(serde_json::from_value::<HashMap<String, ManifestDownloadV2>>)
                .transpose()
                .with_context(|| "failed to parse trialDownloads")?
                .unwrap_or_default()
        } else {
            manifest.downloads.clone()
        };

        let mut entry = entries
            .get(&device)
            .or_else(|| entries.get("default"))
            .or_else(|| entries.values().next())
            .cloned()
            .ok_or_else(|| anyhow!("no downloadable artifact for device `{device}`"))?;

        if entry.display_name.is_none() {
            entry.display_name = self.device_map_id_to_name(&device);
        }

        let base = self.build_repo_cdn_url_by_index_item(&item);
        let resolved_url = if let Some(url) = &entry.url {
            self.resolve_repo_asset_url(&base, url)
        } else {
            format!(
                "{}/{}",
                base.trim_end_matches('/'),
                entry.file_name.trim_start_matches('/')
            )
        };
        entry.url = Some(resolved_url);

        Ok(entry)
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
        let client = crate::net::default_client();

        // 更新index
        let url = (*self.cdn.load_full()).convert_url("https://raw.githubusercontent.com/AstralSightStudios/AstroBox-Repo/refs/heads/main/index_v2.csv");
        let resp = client.get(&url).send().await?.error_for_status()?;
        let raw = resp.bytes().await?;
        let mut list: Vec<IndexV2> = Vec::new();
        let mut csv_read = csv::Reader::from_reader(raw.as_ref());
        for it in csv_read.deserialize::<IndexV2>() {
            if let Ok(mut i) = it {
                if &i.id == "<placeholder>" {
                    let n = self.placeholder_index.load_full().clone();
                    self.placeholder_index.store(Arc::new(*n + 1));
                    i.id = format!("placeholder_{}", n);
                    list.push(i);
                } else {
                    list.push(i);
                }
            }
        }
        self.index.store(Arc::new(list));
        self.split_index(114514, SortRuleV2::Random);

        // 更新设备map
        let url = (*self.cdn.load_full()).convert_url("https://raw.githubusercontent.com/AstralSightStudios/AstroBox-Repo/refs/heads/main/devices_v2.json");
        let resp = client.get(&url).send().await?.error_for_status()?;
        let map: DeviceMapV2 = resp.json().await?;
        self.device_map.store(Arc::new(map));

        // 更新探索页
        let url = (*self.cdn.load_full()).convert_url("https://raw.githubusercontent.com/AstralSightStudios/AstroBox-Repo/refs/heads/main/explore_v2.json");
        let resp = client.get(&url).send().await?.error_for_status()?;
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
        let index = self.index.load().clone();
        let mut filtered_index = (*index).clone();

        // 先根据搜索条件过滤整个索引
        if let Some(categories) = &search.category {
            let hide_paid = categories.contains(&HIDE_PAID.to_string());
            let hide_force_paid = categories.contains(&HIDE_FORCE_PAID.to_string());
            let quick_app = categories.contains(&QUICK_APP.to_string());
            let watchface = categories.contains(&WATCHFACE.to_string());
            let mut devices = Vec::new();

            self.device_map()
                .xiaomi
                .values()
                .filter(|e| categories.contains(&e.name))
                .for_each(|e| {
                    devices.push(e.id.clone());
                });

            let res_type = if quick_app && watchface {
                None
            } else if quick_app {
                Some(ResourceTypeV2::QuickApp)
            } else if watchface {
                Some(ResourceTypeV2::WatchFace)
            } else {
                None
            };

            filtered_index.retain(|item| {
                (item
                    .devices
                    .iter()
                    .any(|category| devices.contains(category))
                    || devices.is_empty())
                    && !(item.paid_type == PaidTypeV2::ForcePaid && hide_force_paid)
                    && !(item.paid_type == PaidTypeV2::Paid && hide_paid)
                    && (if let Some(t) = &res_type {
                        &item.restype == t
                    } else {
                        true
                    })
            });
        }

        if let Some(keyword) = &search.filter {
            filtered_index.retain(|item| {
                item.name.contains(keyword) || item.tags.iter().any(|tag| tag.contains(keyword))
            });
        }

        // 对过滤后的结果进行排序
        // 注意：ThreadRng 非 Send，必须在后续 .await 之前丢弃，故就地取用
        match &search.sort {
            SortRuleV2::Random => filtered_index.shuffle(&mut rand::rng()),
            SortRuleV2::Name => {
                filtered_index.sort_by(|a, b| a.name.cmp(&b.name));
            }
            SortRuleV2::Time => {
                filtered_index.reverse();
            }
        };

        // 对过滤并排序后的结果分页
        let start = (page as usize) * (limit as usize);
        if start >= filtered_index.len() {
            return Ok(Vec::new());
        }

        let end = std::cmp::min(start + limit as usize, filtered_index.len());
        let target_page = &filtered_index[start..end];

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
                paid_type: Some(item.paid_type.clone()),
                restype: item.restype.clone(),

                ..Default::default()
            });
        }

        // 官方镜像源：把本页 icon/cover 经境内 CDN 内联为 base64，避免 webview 直连 GitHub
        if self.cdn.load_full().uses_astrobox_source_cdn() {
            let mut refs = Vec::new();
            for item in target_page.iter() {
                for rel in [item.icon.as_str(), item.cover.as_str()] {
                    if let Some(rel) = Self::relative_image_path(rel) {
                        refs.push(ImageRef {
                            id: item.id.clone(),
                            owner: item.repo_owner.clone(),
                            repo: item.repo_name.clone(),
                            commit: item.repo_commit_hash.clone(),
                            rel,
                        });
                    }
                }
            }
            let inlined = self.inline_images(refs).await;
            if !inlined.is_empty() {
                for (ret_item, idx) in ret.iter_mut().zip(target_page.iter()) {
                    let key = |rel: &str| {
                        Self::image_cos_key(
                            &idx.repo_owner,
                            &idx.repo_name,
                            &idx.repo_commit_hash,
                            rel.trim_start_matches('/'),
                        )
                    };
                    if let Some(data) = inlined.get(&key(&idx.icon)) {
                        ret_item.icon = data.clone();
                    }
                    if let Some(data) = inlined.get(&key(&idx.cover)) {
                        ret_item.cover = data.clone();
                        ret_item.preview = vec![data.clone()];
                    }
                }
            }
        }

        Ok(ret)
    }

    async fn get_categories(&self) -> anyhow::Result<Vec<String>> {
        let mut categories = vec![
            HIDE_PAID.to_string(),
            HIDE_FORCE_PAID.to_string(),
            QUICK_APP.to_string(),
            WATCHFACE.to_string(),
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
            let mut manifest = self
                .get_manifest(&item.repo_owner, &item.repo_name, &item.repo_commit_hash)
                .await?;

            for (device_id, download) in manifest.downloads.iter_mut() {
                download.display_name = self.device_map_id_to_name(device_id);
            }

            let base = self.build_repo_cdn_url_by_index_item(item);
            let mut cover = self.resolve_repo_asset_url(&base, &manifest.item.cover);
            let mut preview = manifest
                .item
                .preview
                .iter()
                .map(|p| self.resolve_repo_asset_url(&base, p))
                .collect::<Vec<_>>();
            let mut icon = self.resolve_repo_asset_url(&base, &item.icon);

            // 官方镜像源：详情页图片同样经境内 CDN 内联为 base64
            if self.cdn.load_full().uses_astrobox_source_cdn() {
                let (owner, repo, commit) = (
                    item.repo_owner.clone(),
                    item.repo_name.clone(),
                    item.repo_commit_hash.clone(),
                );
                let mut refs = Vec::new();
                let rels = std::iter::once(item.icon.as_str())
                    .chain(std::iter::once(manifest.item.cover.as_str()))
                    .chain(manifest.item.preview.iter().map(|s| s.as_str()));
                for rel in rels {
                    if let Some(rel) = Self::relative_image_path(rel) {
                        refs.push(ImageRef {
                            id: item.id.clone(),
                            owner: owner.clone(),
                            repo: repo.clone(),
                            commit: commit.clone(),
                            rel,
                        });
                    }
                }

                let inlined = self.inline_images(refs).await;
                if !inlined.is_empty() {
                    let lookup = |rel: &str| -> Option<String> {
                        let rel = Self::relative_image_path(rel)?;
                        inlined
                            .get(&Self::image_cos_key(&owner, &repo, &commit, &rel))
                            .cloned()
                    };
                    if let Some(data) = lookup(&item.icon) {
                        icon = data;
                    }
                    if let Some(data) = lookup(&manifest.item.cover) {
                        cover = data;
                    }
                    preview = manifest
                        .item
                        .preview
                        .iter()
                        .zip(preview.into_iter())
                        .map(|(rel, fallback)| lookup(rel).unwrap_or(fallback))
                        .collect();
                }
            }

            Ok(ManifestV2 {
                item: ManifestItemV2 {
                    icon,
                    preview,
                    cover,
                    paid_type: Some(item.paid_type.clone()),
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

        // 优先根据id查找，找不到再跟名称
        // 这是为了兼容v1的manifest无id
        let item = index_ref
            .iter()
            .find(|entry| entry.id == item_id)
            .or_else(|| index_ref.iter().find(|entry| entry.name == item_id))
            .cloned()
            .ok_or_else(|| anyhow!("Item not found by id or name"))?;

        let manifest = self
            .get_manifest(&item.repo_owner, &item.repo_name, &item.repo_commit_hash)
            .await
            .with_context(|| format!("failed to fetch manifest for {}", item.name))?;

        let downloads = &manifest.downloads;
        let (resolved_device, download_entry) = downloads
            .get(&device)
            .map(|entry| (device.as_str(), entry))
            .or_else(|| downloads.get("default").map(|entry| ("default", entry)))
            .or_else(|| downloads.iter().next().map(|(key, entry)| (key.as_str(), entry)))
            .map(|(key, entry)| (key.to_string(), entry.clone()))
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

        let safe_file_name = sanitize_local_filename(&file_name);

        let cdn = *self.cdn.load_full();
        let resolved_url = if cdn.uses_astrobox_source_cdn() {
            self.resolve_source_cdn_download_url(&item.id, Some(&resolved_device))
                .await?
        } else if let Some(url) = &download_entry.url {
            cdn.convert_url(url)
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

        let final_path = item_dir.join(&safe_file_name);
        let unique_suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let tmp_path = item_dir.join(format!("{}.{}.part", unique_suffix, safe_file_name));
        let client = crate::net::default_client();
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
                        status: "".into(),
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
                                status: "".into(),
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

    async fn probe_download_size(
        &self,
        item_id: String,
        device: String,
    ) -> anyhow::Result<Option<u64>> {
        let entry = self.resolve_download_entry(item_id, device, false).await?;
        let url = entry.url.clone().context("download url missing")?;
        let client = crate::net::default_client();
        let resp = client.get(&url).send().await?.error_for_status()?;
        Ok(resp.content_length())
    }
}

fn sanitize_local_filename(input: &str) -> String {
    let forbidden = ['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

    let mut s: String = input
        .chars()
        .map(|c| if forbidden.contains(&c) { '_' } else { c })
        .collect();

    s = s.trim().to_string();

    if s.is_empty() || s == "." || s == ".." {
        s = "download".to_string();
    }

    s
}
