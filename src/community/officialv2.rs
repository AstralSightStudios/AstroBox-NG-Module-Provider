use std::{
    cmp,
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, LazyLock, Mutex},
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
use ib_pinyin::{matcher::PinyinMatcher, pinyin::PinyinNotation};
use memchr::memmem::Finder;
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
const GITHUB_TOKEN_CACHE_TTL: Duration = Duration::from_secs(300); // GitHub access_token 内存缓存有效期

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

const COMMUNITY_REPO_OWNER: &str = "AstralSightStudios";
const COMMUNITY_REPO_NAME: &str = "AstroBox-Repo";
const COMMUNITY_REPO_COMMIT: &str = "refs/heads/main";
const COMMUNITY_REPO_INLINE_ID: &str = "__astrobox_community__";

/// 构造社区仓 blogs 目录下的 raw URL（用于相对路径）。
fn build_community_blogs_raw_url(rel: &str) -> String {
    format!(
        "https://raw.githubusercontent.com/{}/{}/{}/blogs/{}",
        COMMUNITY_REPO_OWNER,
        COMMUNITY_REPO_NAME,
        COMMUNITY_REPO_COMMIT,
        rel.trim_start_matches('/')
    )
}

/// 判断 URL 是否属于当前社区 raw 源，并返回仓内相对路径。
fn parse_community_repo_raw_url(url: &str) -> Option<String> {
    let prefix = format!(
        "https://raw.githubusercontent.com/{}/{}/{}/",
        COMMUNITY_REPO_OWNER, COMMUNITY_REPO_NAME, COMMUNITY_REPO_COMMIT
    );
    url.strip_prefix(&prefix).map(|s| s.to_string())
}

/// 判断字符串是否为绝对 URL（含协议或 // 开头）。
fn is_absolute_url(value: &str) -> bool {
    static ABSOLUTE_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:[a-zA-Z][a-zA-Z0-9+.\-]*:|//)").unwrap()
    });
    ABSOLUTE_RE.is_match(value.trim())
}

/// 把探索页资源字段解析为可直接按 CDN 改写的 raw URL。
fn resolve_explore_v2p1_asset_url(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return trimmed.to_owned();
    }
    if is_absolute_url(trimmed) {
        trimmed.to_owned()
    } else {
        build_community_blogs_raw_url(trimmed)
    }
}

/// 移除 JSONC 中的行注释与块注释，保留字符串字面量内容。
fn strip_jsonc_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut in_block = false;
    let mut escape = false;

    while let Some(ch) = chars.next() {
        if in_string {
            out.push(ch);
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if in_block {
            if ch == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_block = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
            out.push(ch);
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'/') {
            chars.next();
            while let Some(c) = chars.next() {
                if c == '\n' {
                    out.push('\n');
                    break;
                }
            }
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            in_block = true;
            continue;
        }
        out.push(ch);
    }
    out
}

/// 移除 JSON 中数组/对象末尾的多余逗号，保留字符串内的逗号。
fn strip_trailing_commas(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escape = false;

    while let Some(ch) = chars.next() {
        if in_string {
            out.push(ch);
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
            out.push(ch);
            continue;
        }
        if ch == ',' {
            let mut found_closer = false;
            let mut cloned = chars.clone();
            while let Some(&c) = cloned.peek() {
                if c.is_whitespace() {
                    cloned.next();
                } else if c == ']' || c == '}' {
                    found_closer = true;
                    break;
                } else {
                    break;
                }
            }
            if found_closer {
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() {
                        chars.next();
                    } else {
                        break;
                    }
                }
                continue;
            }
        }
        out.push(ch);
    }
    out
}

fn parse_jsonc(input: &str) -> anyhow::Result<serde_json::Value> {
    let text = strip_jsonc_comments(input);
    let text = strip_trailing_commas(&text);
    serde_json::from_str(&text).context("failed to parse JSONC as JSON")
}

#[derive(Clone)]
enum PathSegment {
    Key(String),
    Index(usize),
}

struct ExploreAssetRef {
    path: Vec<PathSegment>,
    raw_url: String,
}

fn set_value_at_path(value: &mut serde_json::Value, path: &[PathSegment], new_value: serde_json::Value) {
    let mut current = value;
    if path.is_empty() {
        return;
    }
    for seg in &path[..path.len() - 1] {
        match seg {
            PathSegment::Key(k) => current = current.get_mut(k.as_str()).unwrap(),
            PathSegment::Index(i) => current = current.get_mut(*i).unwrap(),
        }
    }
    match &path[path.len() - 1] {
        PathSegment::Key(k) => current[k.as_str()] = new_value,
        PathSegment::Index(i) => current[*i] = new_value,
    }
}

fn collect_explore_v2p1_assets(value: &serde_json::Value, path: &mut Vec<PathSegment>, out: &mut Vec<ExploreAssetRef>) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                if (k == "backgroundImg" || k == "avatarUrl") && v.is_string() {
                    if let Some(s) = v.as_str() {
                        let raw_url = resolve_explore_v2p1_asset_url(s);
                        let mut asset_path = path.clone();
                        asset_path.push(PathSegment::Key(k.clone()));
                        out.push(ExploreAssetRef { path: asset_path, raw_url });
                    }
                }
                path.push(PathSegment::Key(k.clone()));
                collect_explore_v2p1_assets(v, path, out);
                path.pop();
            }
        }
        serde_json::Value::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                path.push(PathSegment::Index(i));
                collect_explore_v2p1_assets(v, path, out);
                path.pop();
            }
        }
        _ => {}
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
    // 已登录用户在 Raw CDN 下改走 GitHub API 时的 access_token 内存缓存。
    // 三元组：(AstroBox token 标识, GitHub access_token, 获取时间)。
    // 用 AstroBox token 作键，防止同一应用实例切换账号后复用旧用户的 GitHub token。
    github_token_cache: Mutex<Option<(String, String, Instant)>>,
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
            github_token_cache: Mutex::new(None),
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

    /// 把 explore_v2p1.jsonc 解析后的 payload 里所有图片 URL 按当前 CDN 改写。
    /// 当选中 AstroBox Pro 源 CDN 时，社区仓图片会经 `/source-cdn/images`
    /// 签成直链并内联为 base64 data URI，行为与全部资源页/详情页保持一致。
    async fn normalize_explore_v2p1_payload(&self, value: &mut serde_json::Value) -> anyhow::Result<()> {
        let cdn = *self.cdn.load_full();
        let mut refs = Vec::new();
        collect_explore_v2p1_assets(value, &mut Vec::new(), &mut refs);

        let inline_refs: Vec<ImageRef> = if cdn.uses_astrobox_source_cdn() {
            refs.iter()
                .filter(|r| parse_community_repo_raw_url(&r.raw_url).is_some())
                .map(|r| {
                    let rel = parse_community_repo_raw_url(&r.raw_url).unwrap();
                    ImageRef {
                        id: COMMUNITY_REPO_INLINE_ID.to_string(),
                        owner: COMMUNITY_REPO_OWNER.to_string(),
                        repo: COMMUNITY_REPO_NAME.to_string(),
                        commit: COMMUNITY_REPO_COMMIT.to_string(),
                        rel,
                    }
                })
                .collect()
        } else {
            Vec::new()
        };

        let inlined = if inline_refs.is_empty() {
            HashMap::new()
        } else {
            self.inline_images(inline_refs).await
        };

        for r in refs {
            let new_url = if let Some(rel) = parse_community_repo_raw_url(&r.raw_url) {
                let key = Self::image_cos_key(
                    COMMUNITY_REPO_OWNER,
                    COMMUNITY_REPO_NAME,
                    COMMUNITY_REPO_COMMIT,
                    &rel,
                );
                if let Some(data_uri) = inlined.get(&key) {
                    data_uri.clone()
                } else {
                    cdn.convert_asset_url(&r.raw_url)
                }
            } else {
                cdn.convert_asset_url(&r.raw_url)
            };
            set_value_at_path(value, &r.path, serde_json::Value::String(new_url));
        }
        Ok(())
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

    /// 构造「返回给前端展示的图片」base：与 [`build_repo_cdn_url`] 类似，但在「GitHub DoH」下
    /// 走 `astrobox-ghdoh` 协议前缀，使前端原生 `<img>` 经 DoH 客户端回源。
    pub fn build_repo_asset_url(&self, owner: &str, name: &str, commit_hash: &str) -> String {
        let cdn = *self.cdn.load_full();
        cdn.convert_asset_url(&self.build_repo_raw_url(owner, name, commit_hash))
    }

    pub fn build_repo_asset_url_by_index_item(&self, item: &IndexV2) -> String {
        self.build_repo_asset_url(
            &item.repo_owner,
            &item.repo_name,
            &item.repo_commit_hash,
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

    async fn current_github_token(&self) -> anyhow::Result<Option<String>> {
        // 仅 Raw CDN 下才需要 GitHub API token
        if *self.cdn.load_full() != GitHubCdn::Raw {
            return Ok(None);
        }

        let astrobox_token = match self.current_astrobox_token().await {
            Ok(token) => token,
            Err(err) => {
                log::debug!("[OfficialV2] skip github token: astrobox not logged in: {err}");
                return Ok(None);
            }
        };

        {
            if let Ok(cache) = self.github_token_cache.lock() {
                if let Some((key, token, fetched_at)) = cache.as_ref() {
                    if key == &astrobox_token && fetched_at.elapsed() <= GITHUB_TOKEN_CACHE_TTL {
                        return Ok(Some(token.clone()));
                    }
                }
            }
        }

        let base_url = self.current_account_source().await.astrobox_api_base_url();
        let response = match crate::net::default_client()
            .get(format!("{base_url}/auth/api/github-token"))
            .header("X-ASTROBOX-TOKEN", &astrobox_token)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(err) => {
                log::warn!("[OfficialV2] github token request network error: {err}");
                return Ok(None);
            }
        };

        let status = response.status();
        if !status.is_success() {
            log::warn!(
                "[OfficialV2] github token request failed: {status}"
            );
            return Ok(None);
        }

        #[derive(Debug, Deserialize)]
        struct GithubTokenResponse {
            #[serde(rename = "accessToken")]
            access_token: String,
        }

        let payload = match response.json::<GithubTokenResponse>().await {
            Ok(p) => p,
            Err(err) => {
                log::warn!("[OfficialV2] failed to parse github token response: {err}");
                return Ok(None);
            }
        };
        let token = payload.access_token.trim().to_string();
        if token.is_empty() || token == "***" {
            return Ok(None);
        }

        if let Ok(mut cache) = self.github_token_cache.lock() {
            *cache = Some((astrobox_token, token.clone(), Instant::now()));
        }
        Ok(Some(token))
    }

    // 把 raw.githubusercontent.com 地址改写成 GitHub Contents API 地址，
    // 带 ref 参数，请求时配合 Accept: application/vnd.github.raw 即可取回原始字节。
    // 支持 commit hash 以及 `refs/heads/<单段分支名>` 形式的引用。
    // 注意：多段分支名（如 refs/heads/feature/xxx）无法与路径可靠区分，
    // 当前项目实际只使用 commit hash 和 refs/heads/main，故暂不支持。
    fn raw_url_to_github_api(url: &str) -> Option<String> {
        let rest = url.strip_prefix("https://raw.githubusercontent.com/")?;
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() < 4 {
            return None;
        }
        let owner = parts[0];
        let repo = parts[1];

        // raw.githubusercontent.com/{owner}/{repo}/{ref...}/{path...}
        // ref 可能是单段 commit hash，也可能是 refs/heads/<branch> 或 refs/tags/<tag>
        let (git_ref, path_parts) = if parts[2] == "refs" {
            if parts.len() < 6 {
                return None;
            }
            (&parts[2..5], &parts[5..])
        } else {
            (&parts[2..3], &parts[3..])
        };

        let git_ref = git_ref.join("/");
        let path = path_parts
            .iter()
            .map(|s| urlencoding::encode(s))
            .collect::<Vec<_>>()
            .join("/");

        Some(format!(
            "https://api.github.com/repos/{owner}/{repo}/contents/{path}?ref={}",
            urlencoding::encode(&git_ref)
        ))
    }

    // 在 Raw CDN + 已登录且 Casdoor 存了 GitHub access_token 时，
    // 改走 GitHub API（5000/h）以避免 raw.githubusercontent.com 的 429。
    // 条件不满足或任何失败都返回 None，由调用方回退普通 raw 请求。
    async fn try_fetch_via_github_api(
        &self,
        url: &str,
    ) -> anyhow::Result<Option<reqwest::Response>> {
        if *self.cdn.load_full() != GitHubCdn::Raw {
            return Ok(None);
        }
        let Some(token) = self.current_github_token().await? else {
            return Ok(None);
        };
        let Some(api_url) = Self::raw_url_to_github_api(url) else {
            return Ok(None);
        };

        let response = match crate::net::default_client()
            .get(&api_url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/vnd.github.raw")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(err) => {
                log::warn!(
                    "[OfficialV2] GitHub API request failed for {api_url}: {err}; fallback to raw"
                );
                return Ok(None);
            }
        };

        let status = response.status();
        if status.is_success() {
            return Ok(Some(response));
        }

        log::warn!(
            "[OfficialV2] GitHub API returned {status} for {api_url}; fallback to raw"
        );
        Ok(None)
    }

    // 统一 GET 入口：优先尝试 GitHub API（Raw+登录+有 token），失败或条件不满足则走原 URL。
    async fn github_aware_get(&self, url: &str) -> anyhow::Result<reqwest::Response> {
        if let Some(response) = self.try_fetch_via_github_api(url).await? {
            return Ok(response);
        }
        Ok(crate::net::default_client()
            .get(url)
            .send()
            .await?
            .error_for_status()?)
    }

    // 统一 GET 并把响应体读成字节；对 GitCode API（Xuanwu/Jieyuan 数据文件）返回的
    // base64 JSON 包装（{"type":"file","encoding":"base64","content":...}）自动解码回
    // 原始字节。其余源（raw.githubusercontent.com / GitHub API / 前缀代理）响应为纯
    // 内容，原样返回。type=="file" 判别用于排除任意 JSON 恰好含这两键的理论误伤。
    async fn github_aware_bytes(&self, url: &str) -> anyhow::Result<Vec<u8>> {
        let resp = self.github_aware_get(url).await?;
        let bytes = resp.bytes().await?.to_vec();
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            if value.get("type").and_then(|t| t.as_str()) == Some("file")
                && value.get("encoding").and_then(|e| e.as_str()) == Some("base64")
            {
                if let Some(content) = value.get("content").and_then(|c| c.as_str()) {
                    if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(content)
                    {
                        return Ok(decoded);
                    }
                }
            }
        }
        Ok(bytes)
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
        let bytes = self
            .github_aware_bytes(&url)
            .await
            .with_context(|| format!("failed to fetch blog markdown from {}", url))?;
        let text = String::from_utf8_lossy(&bytes).into_owned();

        // Replace naked raw.githubusercontent.com URLs
        let raw_re = Regex::new(
            r#"https://raw\.githubusercontent\.com/AstralSightStudios/AstroBox-Repo/[^)\s\"']+"#,
        )
        .unwrap();
        let text = raw_re.replace_all(&text, |caps: &regex::Captures<'_>| {
            let matched = caps.get(0).unwrap().as_str();
            // 博客正文里的图片/链接按「图片地址」改写（GitCode 镜像下走 raw.gitcode.com
            // 直出，而非 API 的 base64 JSON）。
            cdn.convert_asset_url(matched)
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
                format!("{}({})", prefix, cdn.convert_asset_url(&resolved))
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
        let resp_v2 = match self.try_fetch_via_github_api(&url_v2).await? {
            Some(resp) => resp,
            None => client.get(&url_v2).send().await?,
        };

        if resp_v2.status() == reqwest::StatusCode::NOT_FOUND {
            // fallback v1 manifest
            let url_v1 = format!("{}/manifest.json", base);
            let resp_v1 = match self.try_fetch_via_github_api(&url_v1).await? {
                Some(resp) => resp,
                None => client.get(&url_v1).send().await?,
            };
            let resp_v1 = resp_v1
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

    async fn refresh_inner(&self, cfg: &str) -> anyhow::Result<()> {
        // 更新cdn
        let cfg: HashMap<String, _> = serde_json::from_str(cfg).unwrap_or(HashMap::new());
        let mut cdn: GitHubCdn = *cfg.get("cdn").unwrap_or(&GitHubCdn::Raw);
        // 后端兜底：source-cdn 型 pro 镜像（AstroBoxProMirror /
        // AstroBoxProMirrorWaterFlames）已在产品侧停用，前端仅开通前缀代理型
        // AboxMirror（uses_astrobox_source_cdn = false）。即使前端被绕过/残留
        // 值直通此处，也强制落回 Raw，绝不激活停用的 source-cdn 管线。
        if cdn.uses_astrobox_source_cdn() {
            log::warn!(
                "cdn `{}` is disabled; falling back to Raw",
                cdn.id()
            );
            cdn = GitHubCdn::Raw;
        }
        // 运行时前提兜底：AboxMirror 需要已登录 AstroBox 账号（前端还有 Pro
        // 档位判定，此处只兜「未登录」——覆盖 local_api HTTP 直通 /
        // device/update.rs 直读 localStorage 等绕过前端 sanitize 的旁路）。
        // 会员过期但仍在登录态属于前端守卫职责（后端无 VIP 档位体系）。
        if cdn == GitHubCdn::AboxMirror && self.current_astrobox_token().await.is_err() {
            log::warn!(
                "cdn `AboxMirror` requires a logged-in AstroBox account; falling back to Raw"
            );
            cdn = GitHubCdn::Raw;
        }
        self.cdn.store(Arc::new(cdn));
        // 更新index
        let url = (*self.cdn.load_full()).convert_url("https://raw.githubusercontent.com/AstralSightStudios/AstroBox-Repo/refs/heads/main/index_v2.csv");
        let raw = self
            .github_aware_bytes(&url)
            .await
            .with_context(|| format!("failed to request index_v2.csv from {url}"))?;

        let sanitized = strip_zero_width(&String::from_utf8_lossy(&raw));
        let mut list: Vec<IndexV2> = Vec::new();
        let mut csv_read = csv::ReaderBuilder::new()
            .trim(csv::Trim::All)
            .from_reader(sanitized.as_bytes());
        for it in csv_read.deserialize::<IndexV2>() {
            match it {
                Ok(mut i) => {
                    if &i.id == "<placeholder>" {
                        let n = self.placeholder_index.load_full().clone();
                        self.placeholder_index.store(Arc::new(*n + 1));
                        i.id = format!("placeholder_{}", n);
                        list.push(i);
                    } else {
                        list.push(i);
                    }
                }
                Err(err) => {
                    log::warn!("[OfficialV2] skipped malformed index_v2 row: {err}");
                }
            }
        }
        // 拉到空索引基本意味着响应被 CDN 弄坏了；宁可报错也不要把
        // 已有的良好索引覆盖成空。
        if list.is_empty() {
            anyhow::bail!("index_v2.csv parsed to an empty index");
        }
        self.index.store(Arc::new(list));
        self.split_index(114514, SortRuleV2::Random);

        // 更新设备map
        let url = (*self.cdn.load_full()).convert_url("https://raw.githubusercontent.com/AstralSightStudios/AstroBox-Repo/refs/heads/main/devices_v2.json");
        let bytes = self
            .github_aware_bytes(&url)
            .await
            .with_context(|| format!("failed to request devices_v2.json from {url}"))?;
        let map: DeviceMapV2 = serde_json::from_slice(&bytes)
            .context("failed to parse devices_v2.json")?;
        self.device_map.store(Arc::new(map));

        // 更新探索页
        let url = (*self.cdn.load_full()).convert_url("https://raw.githubusercontent.com/AstralSightStudios/AstroBox-Repo/refs/heads/main/explore_v2p1.jsonc");
        let bytes = self
            .github_aware_bytes(&url)
            .await
            .with_context(|| format!("failed to request explore_v2p1.jsonc from {url}"))?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let mut explore: serde_json::Value = parse_jsonc(&text)
            .with_context(|| format!("failed to parse explore_v2p1.jsonc from {url}"))?;
        self.normalize_explore_v2p1_payload(&mut explore)
            .await
            .context("failed to normalize explore_v2p1.jsonc")?;
        self.explore.store(Arc::new(explore));

        Ok(())
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

        // 失败必须落到 Failed 态：否则 UI 永远停留在 Updating，
        // 既看不到错误也不会触发重试。
        match self.refresh_inner(cfg).await {
            Ok(()) => {
                self.state.store(Arc::new(ProviderState::Ready));
                Ok(())
            }
            Err(err) => {
                log::error!("[OfficialV2] refresh failed: {err:#}");
                self.state
                    .store(Arc::new(ProviderState::Failed(format!("{err:#}"))));
                Err(err)
            }
        }
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
            if !keyword.is_empty() {
                let keyword_lower = keyword.to_ascii_lowercase();
                // memchr: 预编译 needle，循环内零分配子串搜索
                let keyword_finder = Finder::new(keyword_lower.as_bytes());
                // ib-pinyin: 预编译拼音匹配器（简拼 + 全拼），支持中文名称/标签
                // 注意：query 必须小写化——大写字母只匹配字母、不匹配拼音（如 Muyu 匹配不上 木鱼）
                let pinyin_matcher = PinyinMatcher::builder(keyword_lower.as_str())
                    .pinyin_notations(PinyinNotation::Ascii | PinyinNotation::AsciiFirstLetter)
                    .build();

                filtered_index.retain(|item| {
                    // id 前缀匹配（忽略大小写）：如输入 com.searchstars 可搜到 com.searchstars.hyperbilibili
                    if item
                        .id
                        .get(..keyword.len())
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(keyword.as_str()))
                    {
                        return true;
                    }
                    // 快速路径：原始文本 memchr 子串匹配（零分配）
                    if keyword_finder.find(item.name.as_bytes()).is_some()
                        || keyword_finder.find(item.repo_owner.as_bytes()).is_some()
                        || item
                            .tags
                            .iter()
                            .any(|t| keyword_finder.find(t.as_bytes()).is_some())
                    {
                        return true;
                    }
                    // 大小写不敏感路径：小写化后再匹配
                    if keyword_finder
                        .find(item.name.to_ascii_lowercase().as_bytes())
                        .is_some()
                        || keyword_finder
                            .find(item.repo_owner.to_ascii_lowercase().as_bytes())
                            .is_some()
                        || item.tags.iter().any(|t| {
                            keyword_finder
                                .find(t.to_ascii_lowercase().as_bytes())
                                .is_some()
                        })
                    {
                        return true;
                    }
                    // 拼音匹配路径：中文名称/标签支持简拼与全拼搜索
                    pinyin_matcher.is_match(&item.name)
                        || item.tags.iter().any(|t| pinyin_matcher.is_match(t))
                });
            }
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
                    self.build_repo_asset_url_by_index_item(&item),
                    item.cover.clone()
                )],
                icon: format!(
                    "{}/{}",
                    self.build_repo_asset_url_by_index_item(&item),
                    item.icon.clone()
                ),
                cover: format!(
                    "{}/{}",
                    self.build_repo_asset_url_by_index_item(&item),
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

            let base = self.build_repo_asset_url_by_index_item(item);
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
        let response = match self.try_fetch_via_github_api(&resolved_url).await? {
            Some(resp) => resp,
            None => client
                .get(&resolved_url)
                .send()
                .await
                .with_context(|| format!("failed to request {}", resolved_url))?
                .error_for_status()
                .with_context(|| {
                    format!("download request returned error for {}", resolved_url)
                })?,
        };

        let cleanup_path = tmp_path.clone();
        let download_result = {
            let final_path = final_path;
            let tmp_path = tmp_path;
            let progress_cb = progress_cb;
            let response = response;
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
        let resp = self.github_aware_get(&url).await?;
        Ok(resp.content_length())
    }
}

fn strip_zero_width(input: &str) -> String {
    input
        .chars()
        .filter(|c| !matches!(*c, '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{2060}' | '\u{feff}'))
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_url_to_github_api_basic() {
        assert_eq!(
            OfficialV2Provider::raw_url_to_github_api(
                "https://raw.githubusercontent.com/AstralSightStudios/AstroBox-Repo/refs/heads/main/index_v2.csv"
            ),
            Some("https://api.github.com/repos/AstralSightStudios/AstroBox-Repo/contents/index_v2.csv?ref=refs%2Fheads%2Fmain".to_string())
        );
    }

    #[test]
    fn raw_url_to_github_api_with_commit_and_nested_path() {
        assert_eq!(
            OfficialV2Provider::raw_url_to_github_api(
                "https://raw.githubusercontent.com/owner/repo/abc123/manifest_v2.json"
            ),
            Some("https://api.github.com/repos/owner/repo/contents/manifest_v2.json?ref=abc123".to_string())
        );
    }

    #[test]
    fn raw_url_to_github_api_encodes_path() {
        assert_eq!(
            OfficialV2Provider::raw_url_to_github_api(
                "https://raw.githubusercontent.com/owner/repo/abc123/path/with spaces/file.json"
            ),
            Some("https://api.github.com/repos/owner/repo/contents/path/with%20spaces/file.json?ref=abc123".to_string())
        );
    }

    #[test]
    fn raw_url_to_github_api_rejects_non_raw() {
        assert_eq!(
            OfficialV2Provider::raw_url_to_github_api(
                "https://example.com/owner/repo/abc123/file.json"
            ),
            None
        );
    }

    #[test]
    fn raw_url_to_github_api_multi_segment_branch_is_documented_limitation() {
        // 多段分支名无法与路径可靠区分，当前实现按 refs/heads/{第一段} 解析。
        // 这是已知限制；项目实际只使用 commit hash 和 refs/heads/main。
        assert_eq!(
            OfficialV2Provider::raw_url_to_github_api(
                "https://raw.githubusercontent.com/owner/repo/refs/heads/feature/xxx/file.json"
            ),
            Some("https://api.github.com/repos/owner/repo/contents/xxx/file.json?ref=refs%2Fheads%2Ffeature".to_string())
        );
    }

    #[test]
    fn parse_jsonc_strips_comments_and_trailing_commas() {
        let input = r#"{
            // line comment
            "url": "https://example.com//path",
            "cards": [
                { "id": 1, }, /* block */
                { "id": 2, },
            ],
        }"#;
        let value = parse_jsonc(input).unwrap();
        assert_eq!(value["url"], "https://example.com//path");
        assert_eq!(value["cards"][0]["id"], 1);
        assert_eq!(value["cards"][1]["id"], 2);
    }

    #[test]
    fn parse_jsonc_rejects_invalid_json() {
        assert!(parse_jsonc("{ not json").is_err());
    }

    #[test]
    fn is_absolute_url_matches_schemes_and_relative() {
        assert!(is_absolute_url("https://raw.githubusercontent.com/a/b/c.png"));
        assert!(is_absolute_url("data:image/png;base64,xxx"));
        assert!(is_absolute_url("//example.com/x.png"));
        assert!(!is_absolute_url("blogs/hero/a.png"));
        assert!(!is_absolute_url(""));
    }

    #[test]
    fn parse_community_repo_raw_url_roundtrip() {
        let raw = "https://raw.githubusercontent.com/AstralSightStudios/AstroBox-Repo/refs/heads/main/blogs/hero/a.png";
        assert_eq!(
            parse_community_repo_raw_url(raw),
            Some("blogs/hero/a.png".to_string())
        );
        assert_eq!(
            parse_community_repo_raw_url("https://raw.githubusercontent.com/other/repo/main/x.png"),
            None
        );
    }

    #[test]
    fn resolve_explore_v2p1_asset_url_resolves_relative() {
        assert_eq!(
            resolve_explore_v2p1_asset_url("hero/a.png"),
            "https://raw.githubusercontent.com/AstralSightStudios/AstroBox-Repo/refs/heads/main/blogs/hero/a.png"
        );
        assert_eq!(
            resolve_explore_v2p1_asset_url("https://cdn.example.com/a.png"),
            "https://cdn.example.com/a.png"
        );
        assert_eq!(resolve_explore_v2p1_asset_url(""), "");
    }

    #[test]
    fn set_value_at_path_nested_keys_and_indexes() {
        let mut value = serde_json::json!({
            "sections": [ { "cards": [ { "backgroundImg": "old.png" } ] } ]
        });
        let path = vec![
            PathSegment::Key("sections".into()),
            PathSegment::Index(0),
            PathSegment::Key("cards".into()),
            PathSegment::Index(0),
            PathSegment::Key("backgroundImg".into()),
        ];
        set_value_at_path(&mut value, &path, serde_json::Value::String("new.png".into()));
        assert_eq!(value["sections"][0]["cards"][0]["backgroundImg"], "new.png");
    }

    #[test]
    fn collect_explore_v2p1_assets_finds_asset_fields() {
        let value = serde_json::json!({
            "customSections": [],
            "sections": [
                {
                    "cards": [
                        { "type": "blog", "backgroundImg": "hero/a.png", "url": "https://x" },
                        { "type": "author", "avatarUrl": "authors/b.png" },
                        { "type": "resource", "resourceId": "r1" },
                    ]
                }
            ]
        });
        let mut refs = Vec::new();
        collect_explore_v2p1_assets(&value, &mut Vec::new(), &mut refs);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].raw_url, "https://raw.githubusercontent.com/AstralSightStudios/AstroBox-Repo/refs/heads/main/blogs/hero/a.png");
        assert_eq!(refs[1].raw_url, "https://raw.githubusercontent.com/AstralSightStudios/AstroBox-Repo/refs/heads/main/blogs/authors/b.png");
    }

    #[test]
    fn pinyin_matcher_matches_chinese_names() {
        use ib_pinyin::{matcher::PinyinMatcher, pinyin::PinyinNotation};
        fn build(q: &str) -> PinyinMatcher<'_> {
            PinyinMatcher::builder(q)
                .pinyin_notations(PinyinNotation::Ascii | PinyinNotation::AsciiFirstLetter)
                .build()
        }
        // 全拼
        assert!(build("muyu").is_match("木鱼Pro"));
        // 简拼
        assert!(build("my").is_match("木鱼Pro"));
        // 中英混合（全拼 + 字母）
        assert!(build("muyupro").is_match("木鱼Pro"));
        // 无关拼音不匹配
        assert!(!build("muma").is_match("木鱼Pro"));
        // 大小写混合/大写 query（如输入法自动大写）必须先小写化才能命中拼音
        for raw in ["Muyu", "MUYU", "MuyuPro"] {
            let query = raw.to_ascii_lowercase();
            assert!(build(&query).is_match("木鱼Pro"), "raw query: {raw}");
        }
        // 中文 query 不误伤纯英文资源名
        assert!(!build("木鱼").is_match("MiBand 8"));
    }

    #[test]
    fn id_prefix_matching_semantics() {
        // 与 get_page 一致的 id 前缀匹配逻辑（忽略大小写、UTF-8 安全切片）
        let matches = |id: &str, kw: &str| {
            id.get(..kw.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(kw))
        };
        assert!(matches("com.searchstars.hyperbilibili", "com.searchstars"));
        assert!(matches("COM.SearchStars.HyperBiliBili", "com.searchstars"));
        assert!(matches("LegacyItem1", "legacy"));
        assert!(!matches("com.a.b", "com.searchstars"));
        assert!(!matches("abc", "abcd"));
        // 中文字节安全：不会因切片落在字符中间而 panic
        assert!(!matches("abc", "木鱼"));
    }
}
