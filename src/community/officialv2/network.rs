use super::*;
use futures_util::stream::FuturesUnordered;
use netcfg::download::DownloadSource;
use reqwest::Client;

const METADATA_LIMIT: usize = 8 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const OPERATION_TIMEOUT: Duration = Duration::from_secs(12);
const HEDGE_DELAY: Duration = Duration::from_millis(800);

#[derive(Default)]
pub(super) struct Network {
    clients: Mutex<Option<(netcfg::DohConfig, Client, Client)>>,
    download_clients: Mutex<Option<(netcfg::DohConfig, Client, Client)>>,
    health: Mutex<HashMap<&'static str, (u32, Duration, Instant)>>,
}

impl Network {
    pub(super) fn client(&self, cdn: GitHubCdn) -> Client {
        self.route_client(cdn, false)
    }
    pub(super) fn download_client(&self, cdn: GitHubCdn) -> Client {
        self.route_client(cdn, true)
    }

    fn route_client(&self, cdn: GitHubCdn, download: bool) -> Client {
        let config = netcfg::get_doh_config();
        let mut clients = (if download {
            &self.download_clients
        } else {
            &self.clients
        })
        .lock()
        .unwrap_or_else(|e| e.into_inner());
        if clients.as_ref().is_none_or(|(old, _, _)| *old != config) {
            let build = |doh| {
                netcfg::github_client_builder(doh)
                    .redirect(if download {
                        reqwest::redirect::Policy::none()
                    } else {
                        reqwest::redirect::Policy::limited(5)
                    })
                    .no_gzip()
                    .no_brotli()
                    .no_deflate()
                    .no_zstd()
                    .build()
                    .expect("failed to create provider client")
            };
            *clients = Some((config, build(false), build(true)));
        }
        let (_, raw, doh) = clients.as_ref().unwrap();
        if cdn.uses_github_doh() {
            doh.clone()
        } else {
            raw.clone()
        }
    }

    fn record(&self, cdn: GitHubCdn, elapsed: Duration, success: bool) {
        let mut health = self.health.lock().unwrap_or_else(|e| e.into_inner());
        let failures = if success {
            0
        } else {
            health
                .get(cdn.id())
                .map(|s| s.0)
                .unwrap_or(0)
                .saturating_add(1)
        };
        health.insert(cdn.id(), (failures, elapsed, Instant::now()));
    }

    pub(super) fn routes(&self, raw_url: &str, selected: GitHubCdn) -> Vec<(GitHubCdn, String)> {
        if !crate::cdn::is_convertible_github_url(raw_url) {
            return vec![(selected, raw_url.to_string())];
        }
        let mut fallback = vec![
            GitHubCdn::GhFast,
            GitHubCdn::GhProxy,
            GitHubCdn::GhProxyOrg,
            GitHubCdn::GitHubDoh,
            GitHubCdn::Raw,
            GitHubCdn::GhDdlc,
            GitHubCdn::Isteed,
        ];
        let health = self.health.lock().unwrap_or_else(|e| e.into_inner());
        fallback.sort_by_key(|cdn| match health.get(cdn.id()) {
            Some((failures, elapsed, time)) if time.elapsed() < Duration::from_secs(120) => {
                (*failures, *elapsed)
            }
            _ => (0, Duration::from_secs(2)),
        });
        let selected_cooling_down = health
            .get(selected.id())
            .is_some_and(|(failures, _, time)| {
                *failures >= 2 && time.elapsed() < Duration::from_secs(30)
            });
        drop(health);
        let mut ordered = if selected_cooling_down {
            fallback.clone()
        } else {
            vec![selected]
        };
        if selected_cooling_down {
            ordered.push(selected);
        } else {
            ordered.extend(fallback);
        }
        let mut routes = Vec::new();
        for cdn in ordered {
            let url = cdn.convert_url(raw_url);
            // Non-GitHub addresses must not be retried as imaginary mirrors.
            let doh = cdn.uses_github_doh();
            if routes.iter().any(|(old, old_url): &(GitHubCdn, String)| {
                old_url == &url && old.uses_github_doh() == doh
            }) {
                continue;
            }
            routes.push((cdn, url));
        }
        routes.truncate(4);
        routes
    }

    pub(super) fn downloads(
        &self,
        raw_url: &str,
        cdn: GitHubCdn,
        immutable: bool,
    ) -> Vec<DownloadSource> {
        self.routes(raw_url, cdn)
            .into_iter()
            .filter(|(_, url)| !url.starts_with("https://api.gitcode.com/"))
            .map(|(cdn, url)| {
                let mut source = DownloadSource::new(self.route_client(cdn, true), url);
                source.immutable = immutable;
                source
            })
            .collect()
    }
}

#[derive(Debug)]
struct MetadataFailure {
    not_found: bool,
    message: String,
}
impl std::fmt::Display for MetadataFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}
impl std::error::Error for MetadataFailure {}
pub(super) fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<MetadataFailure>()
        .is_some_and(|e| e.not_found)
}

impl OfficialV2Provider {
    pub(super) async fn fetch_metadata<T: Send>(
        &self,
        raw_url: &str,
        cdn: GitHubCdn,
        parse: impl Fn(&[u8]) -> anyhow::Result<T> + Sync,
    ) -> anyhow::Result<T> {
        let mut routes = self.network.routes(raw_url, cdn).into_iter();
        let mut requests = FuturesUnordered::new();
        if let Some((cdn, url)) = routes.next() {
            requests.push(self.metadata_attempt(cdn, url, &parse));
        }
        let deadline = tokio::time::sleep(OPERATION_TIMEOUT);
        let hedge = tokio::time::sleep(HEDGE_DELAY);
        tokio::pin!(deadline, hedge);
        let mut hedged = false;
        let mut failures = Vec::new();
        let mut not_found_count = 0;
        let mut authoritative_not_found = false;
        loop {
            tokio::select! {
                _ = &mut deadline => { failures.push("metadata request deadline exceeded".to_string()); break; }
                _ = &mut hedge, if !hedged => {
                    hedged = true;
                    if let Some((cdn, url)) = routes.next() { requests.push(self.metadata_attempt(cdn, url, &parse)); }
                }
                result = requests.next() => {
                    let Some((route, result)) = result else { break };
                    match result {
                        Ok(value) => return Ok(value),
                        Err(error) => {
                            if is_not_found(&error) {
                                not_found_count += 1;
                                authoritative_not_found |= matches!(route, GitHubCdn::Raw | GitHubCdn::GitHubDoh);
                            }
                            failures.push(format!("{}: {error:#}", route.id()));
                            if let Some((cdn, url)) = routes.next() { requests.push(self.metadata_attempt(cdn, url, &parse)); }
                        }
                    }
                }
            }
        }
        Err(MetadataFailure {
            not_found: authoritative_not_found
                || (not_found_count > 0 && not_found_count == failures.len()),
            message: failures.join("; "),
        }
        .into())
    }

    async fn metadata_attempt<T: Send>(
        &self,
        cdn: GitHubCdn,
        url: String,
        parse: &(impl Fn(&[u8]) -> anyhow::Result<T> + Sync),
    ) -> (GitHubCdn, anyhow::Result<T>) {
        let started = Instant::now();
        let result = tokio::time::timeout(REQUEST_TIMEOUT, async {
            let mut response = self.github_aware_send(&url, cdn).await?;
            if response.status() == StatusCode::NOT_FOUND {
                return Err(MetadataFailure {
                    not_found: true,
                    message: "HTTP 404".to_string(),
                }
                .into());
            }
            response = response.error_for_status()?;
            if response.status() != StatusCode::OK {
                anyhow::bail!("unexpected metadata status {}", response.status());
            }
            if response
                .content_length()
                .is_some_and(|n| n > METADATA_LIMIT as u64)
            {
                anyhow::bail!("metadata response too large");
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                if bytes.len().saturating_add(chunk.len()) > METADATA_LIMIT {
                    anyhow::bail!("metadata response too large");
                }
                bytes.extend_from_slice(&chunk);
            }
            let bytes = decode_gitcode(bytes)?;
            parse(&bytes)
        })
        .await
        .unwrap_or_else(|_| Err(anyhow!("metadata request timed out")));
        self.network.record(cdn, started.elapsed(), result.is_ok());
        (cdn, result)
    }
}

fn decode_gitcode(bytes: Vec<u8>) -> anyhow::Result<Vec<u8>> {
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
        if value.get("type").and_then(|t| t.as_str()) == Some("file")
            && value.get("encoding").and_then(|e| e.as_str()) == Some("base64")
        {
            let content = value
                .get("content")
                .and_then(|c| c.as_str())
                .context("GitCode response lacks content")?;
            let content: String = content
                .chars()
                .filter(|c| !c.is_ascii_whitespace())
                .collect();
            return base64::engine::general_purpose::STANDARD
                .decode(content)
                .context("invalid GitCode base64 response");
        }
    }
    Ok(bytes)
}

pub(crate) fn parse_index(bytes: &[u8]) -> anyhow::Result<Vec<IndexV2>> {
    let text = strip_zero_width(std::str::from_utf8(bytes)?.trim_start_matches('\u{feff}'));
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(text.as_bytes());
    let headers = reader.headers()?;
    for required in [
        "id",
        "name",
        "restype",
        "repo_owner",
        "repo_name",
        "repo_commit_hash",
        "icon",
        "cover",
        "tags",
        "device_vendors",
        "devices",
        "paid_type",
    ] {
        if !headers.iter().any(|h| h == required) {
            anyhow::bail!("index_v2.csv missing column {required}");
        }
    }
    let mut list = Vec::new();
    for (row, item) in reader.deserialize::<IndexV2>().enumerate() {
        // 单行格式错误只跳过该行，不能让一行坏数据拖垮整个目录。
        let mut item = match item {
            Ok(item) => item,
            Err(err) => {
                log::warn!("[OfficialV2] skipped malformed index_v2 row: {err}");
                continue;
            }
        };
        if item.id.is_empty()
            || item.repo_owner.is_empty()
            || item.repo_name.is_empty()
            || item.repo_commit_hash.is_empty()
        {
            log::warn!(
                "[OfficialV2] skipped index_v2 row {} without resource identity",
                row + 2
            );
            continue;
        }
        if item.id == "<placeholder>" {
            item.id = format!("placeholder_{row}");
        }
        list.push(item);
    }
    if list.is_empty() {
        anyhow::bail!("index_v2.csv parsed to an empty index");
    }
    Ok(list)
}

pub(super) fn parse_devices(bytes: &[u8]) -> anyhow::Result<DeviceMapV2> {
    let map: DeviceMapV2 = serde_json::from_slice(bytes)?;
    if map.xiaomi.is_empty() && map.vivo.is_empty() {
        anyhow::bail!("empty device map");
    }
    Ok(map)
}

pub(super) fn parse_explore(bytes: &[u8]) -> anyhow::Result<serde_json::Value> {
    let value = parse_jsonc(std::str::from_utf8(bytes)?)?;
    if !value.get("sections").is_some_and(|v| v.is_array()) {
        anyhow::bail!("explore payload lacks sections");
    }
    Ok(value)
}

pub(super) fn parse_manifest(bytes: &[u8]) -> anyhow::Result<ManifestV2> {
    Ok(serde_json::from_slice(bytes)?)
}

pub(super) fn parse_markdown(bytes: &[u8]) -> anyhow::Result<String> {
    let text = std::str::from_utf8(bytes)?;
    let trimmed = text.trim_start().to_ascii_lowercase();
    if trimmed.starts_with("<!doctype html") || trimmed.starts_with("<html") {
        anyhow::bail!("received an HTML error page instead of markdown");
    }
    Ok(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_validation_rejects_successful_error_pages_and_empty_indexes() {
        assert!(parse_index(b"<html>error</html>").is_err());
        assert!(parse_index(b"id,name\na,b").is_err());
        let header = "id,name,restype,repo_owner,repo_name,repo_commit_hash,icon,cover,tags,device_vendors,devices,paid_type\n";
        let good = "a,A,watchface,o,r,abc1234,i.png,c.png,t,xiaomi,xmb10,\n";
        let bad = "b,B,unknown_type,o,r,abc1234,i.png,c.png,t,xiaomi,xmb10,\n";
        assert_eq!(
            parse_index(format!("{header}{bad}{good}").as_bytes())
                .unwrap()
                .len(),
            1
        );
        assert!(parse_index(format!("{header}{bad}").as_bytes()).is_err());
        assert!(parse_explore(b"{}").is_err());
        assert!(parse_markdown(b"<!doctype html><html>bad gateway</html>").is_err());
    }
    #[test]
    fn gitcode_base64_can_contain_newlines() {
        assert_eq!(
            decode_gitcode(
                br#"{"type":"file","encoding":"base64","content":"aG\nVsbG8="}"#.to_vec()
            )
            .unwrap(),
            b"hello"
        );
    }
    #[test]
    fn automatic_routes_do_not_reenable_disabled_or_manual_only_mirrors() {
        let network = Network::default();
        let routes = network.routes(
            "https://raw.githubusercontent.com/owner/repo/hash/data.json",
            GitHubCdn::Raw,
        );
        assert_eq!(routes[0].0, GitHubCdn::Raw);
        assert!(routes.iter().all(|(cdn, _)| !matches!(
            cdn,
            GitHubCdn::Xuanwu
                | GitHubCdn::Jieyuan
                | GitHubCdn::AboxMirror
                | GitHubCdn::AstroBoxProMirror
                | GitHubCdn::AstroBoxProMirrorWaterFlames
        )));
        assert!(routes.len() <= 4);
    }
}
