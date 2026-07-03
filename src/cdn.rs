use serde::{Deserialize, Serialize};
use std::sync::{LazyLock, RwLock};

/// 「GitHub DoH」自定义协议前缀（如 `astrobox-ghdoh://localhost/`），由 app 层在启动时按平台注入。
/// 用于把返回给前端展示的 GitHub 图片地址改写到本地协议，交由后端经 DoH 客户端回源。
static GHDOH_BASE: LazyLock<RwLock<Option<String>>> = LazyLock::new(|| RwLock::new(None));

/// 注入「GitHub DoH」自定义协议前缀（app 层启动时调用一次）。
pub fn set_ghdoh_base(base: String) {
    if let Ok(mut guard) = GHDOH_BASE.write() {
        *guard = Some(base);
    }
}

fn ghdoh_base() -> Option<String> {
    GHDOH_BASE.read().ok().and_then(|guard| guard.clone())
}

/// GitHub 资源加速 CDN。
///
/// 前缀型代理镜像（[`GitHubCdn::proxy_prefix`] 返回 `Some`）通过在原始 URL 前拼接代理域名
/// 实现加速，同时支持 `raw.githubusercontent.com` 与 `github.com` 的 release / 归档下载。
///
/// 前端存在一份等价实现（`web/src/logic/githubCdn.ts`），新增或调整镜像时两处需保持一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitHubCdn {
    Raw,
    /// GitHub 域名走 DoH 解析后直连（不经第三方代理）。URL 不改写，加速在传输层由
    /// netcfg 的 DoH 客户端完成；前端原生 `<img>` 通过 `astrobox-ghdoh` 自定义协议回落到该客户端。
    GitHubDoh,
    AstroBoxProMirror,
    AstroBoxProMirrorWaterFlames,
    GhFast,
    GhProxy,
    GhProxyOrg,
    GhDdlc,
    Isteed,
}

impl GitHubCdn {
    /// 社区可选 CDN（含中国大陆赞助镜像，用于测速；赞助项是否展示由前端控制）。
    pub const ALL: [Self; 8] = [
        GitHubCdn::Raw,
        GitHubCdn::GitHubDoh,
        GitHubCdn::AstroBoxProMirrorWaterFlames,
        GitHubCdn::GhFast,
        GitHubCdn::GhProxy,
        GitHubCdn::GhProxyOrg,
        GitHubCdn::GhDdlc,
        GitHubCdn::Isteed,
    ];

    /// 序列化标识（与 serde 变体名一致，前端以此作为 CDN id）。
    pub fn id(self) -> &'static str {
        match self {
            GitHubCdn::Raw => "Raw",
            GitHubCdn::GitHubDoh => "GitHubDoh",
            GitHubCdn::AstroBoxProMirror => "AstroBoxProMirror",
            GitHubCdn::AstroBoxProMirrorWaterFlames => "AstroBoxProMirrorWaterFlames",
            GitHubCdn::GhFast => "GhFast",
            GitHubCdn::GhProxy => "GhProxy",
            GitHubCdn::GhProxyOrg => "GhProxyOrg",
            GitHubCdn::GhDdlc => "GhDdlc",
            GitHubCdn::Isteed => "Isteed",
        }
    }

    /// 是否为「GitHub DoH」传输层加速（不改写 URL，靠 DoH 客户端解析）。
    pub fn uses_github_doh(self) -> bool {
        matches!(self, GitHubCdn::GitHubDoh)
    }

    pub fn normalized(self) -> Self {
        match self {
            GitHubCdn::AstroBoxProMirror => GitHubCdn::AstroBoxProMirrorWaterFlames,
            other => other,
        }
    }

    /// 前缀型代理镜像的 URL 前缀；直连（`Raw`）与赞助镜像返回 `None`。
    fn proxy_prefix(self) -> Option<&'static str> {
        match self.normalized() {
            GitHubCdn::GhFast => Some("https://ghfast.top/"),
            GitHubCdn::GhProxy => Some("https://gh-proxy.com/"),
            GitHubCdn::GhProxyOrg => Some("https://gh-proxy.org/"),
            GitHubCdn::GhDdlc => Some("https://gh.ddlc.top/"),
            GitHubCdn::Isteed => Some("https://cors.isteed.cc/"),
            GitHubCdn::Raw
            | GitHubCdn::GitHubDoh
            | GitHubCdn::AstroBoxProMirror
            | GitHubCdn::AstroBoxProMirrorWaterFlames => None,
        }
    }

    pub fn convert_url(self, url: &str) -> String {
        if !is_convertible_github_url(url) {
            return url.to_owned();
        }

        match self.proxy_prefix() {
            Some(prefix) => format!("{prefix}{}", url.strip_prefix("https://").unwrap_or(url)),
            None => url.to_owned(),
        }
    }

    /// 用于「返回给前端展示的图片地址」的改写。除「GitHub DoH」外与 [`convert_url`] 一致；
    /// 「GitHub DoH」下改写为本地 `astrobox-ghdoh` 协议前缀（前端原生 `<img>` 由此走 DoH 回源），
    /// 而不是直连 raw（否则 webview 直连 GitHub 无法走 DoH）。
    pub fn convert_asset_url(self, url: &str) -> String {
        if self.uses_github_doh() {
            if is_convertible_github_url(url) {
                if let Some(base) = ghdoh_base() {
                    return format!("{base}{}", url.strip_prefix("https://").unwrap_or(url));
                }
            }
            return url.to_owned();
        }
        self.convert_url(url)
    }

    pub fn uses_astrobox_source_cdn(self) -> bool {
        matches!(self.normalized(), GitHubCdn::AstroBoxProMirrorWaterFlames)
    }

    pub fn probe_url(self, fallback_raw_url: &str) -> String {
        if self.uses_astrobox_source_cdn() {
            "https://abpromirror.waterflames.cn/".to_string()
        } else {
            self.convert_url(fallback_raw_url)
        }
    }

    pub fn get_cdns() -> Vec<String> {
        Self::ALL.iter().map(|item| item.id().to_string()).collect()
    }
}

/// 判断 URL 是否为可经镜像加速的 GitHub 资源：
/// `raw.githubusercontent.com`/`gist.githubusercontent.com` 原始文件，或 `github.com` 的
/// release 下载与源码归档。
fn is_convertible_github_url(url: &str) -> bool {
    if url.starts_with("https://raw.githubusercontent.com/")
        || url.starts_with("https://gist.githubusercontent.com/")
    {
        return true;
    }

    if let Some(rest) = url.strip_prefix("https://github.com/") {
        return rest.contains("/releases/download/")
            || rest.contains("/releases/latest/download/")
            || rest.contains("/archive/");
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_passthrough_for_direct() {
        let url = "https://raw.githubusercontent.com/owner/repo/main/index.csv";
        assert_eq!(GitHubCdn::Raw.convert_url(url), url);
    }

    #[test]
    fn proxy_prefix_rewrites_raw() {
        let url = "https://raw.githubusercontent.com/owner/repo/main/index.csv";
        assert_eq!(
            GitHubCdn::GhProxyOrg.convert_url(url),
            "https://gh-proxy.org/raw.githubusercontent.com/owner/repo/main/index.csv"
        );
    }

    #[test]
    fn proxy_prefix_rewrites_release_download() {
        let url = "https://github.com/owner/repo/releases/download/v1/app.bin";
        assert_eq!(
            GitHubCdn::GhDdlc.convert_url(url),
            "https://gh.ddlc.top/github.com/owner/repo/releases/download/v1/app.bin"
        );
    }

    #[test]
    fn non_github_url_untouched() {
        let url = "https://example.com/file.bin";
        assert_eq!(GitHubCdn::Isteed.convert_url(url), url);
    }

    #[test]
    fn github_doh_keeps_raw_url() {
        // GitHub DoH 在传输层加速，不改写 URL。
        let url = "https://raw.githubusercontent.com/owner/repo/main/index.csv";
        assert_eq!(GitHubCdn::GitHubDoh.convert_url(url), url);
        assert!(GitHubCdn::GitHubDoh.uses_github_doh());
        assert!(!GitHubCdn::Raw.uses_github_doh());
    }

    #[test]
    fn pro_mirror_does_not_rewrite() {
        let url = "https://raw.githubusercontent.com/owner/repo/main/index.csv";
        assert_eq!(GitHubCdn::AstroBoxProMirrorWaterFlames.convert_url(url), url);
    }

    #[test]
    fn cdn_ids_match_serde() {
        for cdn in GitHubCdn::ALL {
            let serde_id = serde_json::to_string(&cdn).unwrap();
            assert_eq!(format!("\"{}\"", cdn.id()), serde_id);
        }
    }

    #[test]
    fn github_doh_asset_uses_ghdoh_base() {
        let url = "https://raw.githubusercontent.com/o/r/main/icon.png";
        set_ghdoh_base("astrobox-ghdoh://localhost/".to_string());
        assert_eq!(
            GitHubCdn::GitHubDoh.convert_asset_url(url),
            "astrobox-ghdoh://localhost/raw.githubusercontent.com/o/r/main/icon.png"
        );
        // 代理镜像的图片改写与普通改写一致；非 GitHub 地址不动。
        assert_eq!(
            GitHubCdn::GhProxy.convert_asset_url(url),
            GitHubCdn::GhProxy.convert_url(url)
        );
        assert_eq!(
            GitHubCdn::GitHubDoh.convert_asset_url("https://example.com/x.png"),
            "https://example.com/x.png"
        );
    }
}
