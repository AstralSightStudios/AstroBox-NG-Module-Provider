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

/// GitCode 镜像仓（[`GitHubCdn::Xuanwu`]/[`GitHubCdn::Jieyuan`]）仅镜像了官方源仓
/// `AstralSightStudios/AstroBox-Repo`。仅该前缀的原始文件存在于镜像仓中；第三方资源仓库
/// 的文件（资源详情 manifest、固件、插件等）不在镜像仓内，改写过去必然 404，须保持 GitHub 直连。
const GITCODE_MIRRORED_REPO_PREFIX: &str =
    "https://raw.githubusercontent.com/AstralSightStudios/AstroBox-Repo/";

/// GitCode 镜像仓信息。数据文件（文本，可能 >30KB——`raw.gitcode.com` 对较大文件返回
/// 403「暂不支持预览」）改写为 GitCode API contents 接口并解码 base64；小文件（图片等）
/// 仍走 `raw.gitcode.com` 直出。
struct GitCodeMirror {
    /// GitCode API contents 前缀（数据文件 URL 基座，尾部拼 `?format=raw&ref=<branch>`）。
    api_prefix: &'static str,
    /// `raw.gitcode.com` 前缀（小文件直出，用于图片等二进制）。
    raw_prefix: &'static str,
    /// 镜像仓分支。
    branch: &'static str,
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
    /// host 替换型赞助镜像（数据链路与 Raw 一致）：`mirror.abox.run` 直接替换
    /// `raw.githubusercontent.com` 域名，而非在完整 URL 前拼接代理前缀。
    /// Pro 会员门槛仅由前端拦截（isOfficialProCdn 锁定 + 设置页 disabled），后端无 VIP 校验——
    /// 与 WaterFlames 不同（后者走 /source-cdn 由 AstroBox 服务端 403 兜底）。
    /// 已知妥协：非 Pro 用户若直接改写持久化配置绕过前端门槛，mirror.abox.run
    /// 是否自行鉴权由该外部服务决定，本仓库无法兜底。
    AboxMirror,
    GhFast,
    GhProxy,
    GhProxyOrg,
    GhDdlc,
    Isteed,
    /// GitCode API 型免费镜像：数据文件改写为 GitCode API contents 接口
    /// （`raw.gitcode.com` 对较大文件返回 403「暂不支持预览」，API 返回 base64 JSON，
    /// 由拉取层 `github_aware_bytes` 解码）；小文件（图片等）走 `raw.gitcode.com` 直出。
    /// 免费开放，无 Pro 门槛；数据链路与 Raw 一致。
    Xuanwu,
    /// GitCode API 型免费镜像：`Bikboke/abmirror` 镜像仓，行为同 [`GitHubCdn::Xuanwu`]。
    /// 免费开放，无 Pro 门槛；数据链路与 Raw 一致。
    Jieyuan,
}

impl GitHubCdn {
    /// 社区可选 CDN（用于测速 / 自动切换）。pro 镜像（AstroBoxProMirror /
    /// AstroBoxProMirrorWaterFlames / AboxMirror）已全部停用，不在列表中。
    /// Xuanwu / Jieyuan 仍保留可手动选择，但被排除在自动测速与自动选择最佳线路之外
    /// （前端 AUTO_SELECTION_EXCLUDED_OFFICIAL_CDNS + 后端 network_run_speed_test 跳过）。
    pub const ALL: [Self; 9] = [
        GitHubCdn::Raw,
        GitHubCdn::GitHubDoh,
        GitHubCdn::GhFast,
        GitHubCdn::GhProxy,
        GitHubCdn::GhProxyOrg,
        GitHubCdn::GhDdlc,
        GitHubCdn::Isteed,
        GitHubCdn::Xuanwu,
        GitHubCdn::Jieyuan,
    ];

    /// 序列化标识（与 serde 变体名一致，前端以此作为 CDN id）。
    pub fn id(self) -> &'static str {
        match self {
            GitHubCdn::Raw => "Raw",
            GitHubCdn::GitHubDoh => "GitHubDoh",
            GitHubCdn::AstroBoxProMirror => "AstroBoxProMirror",
            GitHubCdn::AstroBoxProMirrorWaterFlames => "AstroBoxProMirrorWaterFlames",
            GitHubCdn::AboxMirror => "AboxMirror",
            GitHubCdn::GhFast => "GhFast",
            GitHubCdn::GhProxy => "GhProxy",
            GitHubCdn::GhProxyOrg => "GhProxyOrg",
            GitHubCdn::GhDdlc => "GhDdlc",
            GitHubCdn::Isteed => "Isteed",
            GitHubCdn::Xuanwu => "Xuanwu",
            GitHubCdn::Jieyuan => "Jieyuan",
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

    /// 前缀型代理镜像的 URL 前缀；直连（`Raw`）、DoH、source-cdn 型赞助镜像、
    /// host 替换型（`AboxMirror`）与 GitCode API 型（`Xuanwu`/`Jieyuan`）返回 `None`。
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
            | GitHubCdn::AstroBoxProMirrorWaterFlames
            | GitHubCdn::AboxMirror
            | GitHubCdn::Xuanwu
            | GitHubCdn::Jieyuan => None,
        }
    }

    /// Host 替换型镜像的 (旧域名前缀, 新域名前缀)：直接把 `raw.githubusercontent.com` 域名
    /// 替换为镜像域名（如 `https://mirror.abox.run/owner/repo/...`），而非在完整 URL 前
    /// 拼接代理前缀。目前仅 `AboxMirror`（赞助）命中，其余返回 `None`
    /// （`Xuanwu`/`Jieyuan` 的 `raw.gitcode.com` 前缀见 [`GitHubCdn::gitcode_mirror`]）。
    fn host_rewrite(self) -> Option<(&'static str, &'static str)> {
        match self.normalized() {
            GitHubCdn::AboxMirror => {
                Some(("https://raw.githubusercontent.com/", "https://mirror.abox.run/"))
            }
            _ => None,
        }
    }

    /// GitCode 镜像仓信息（`Xuanwu`/`Jieyuan` 命中，其余 `None`）。
    fn gitcode_mirror(self) -> Option<GitCodeMirror> {
        match self.normalized() {
            GitHubCdn::Xuanwu => Some(GitCodeMirror {
                api_prefix:
                    "https://api.gitcode.com/api/v5/repos/gcw_MdSkpmRq/AstroBox-Repo-Mirror/contents/",
                raw_prefix:
                    "https://raw.gitcode.com/gcw_MdSkpmRq/AstroBox-Repo-Mirror/raw/main/",
                branch: "main",
            }),
            GitHubCdn::Jieyuan => Some(GitCodeMirror {
                api_prefix:
                    "https://api.gitcode.com/api/v5/repos/Bikboke/abmirror/contents/",
                raw_prefix: "https://raw.gitcode.com/Bikboke/abmirror/raw/main/",
                branch: "main",
            }),
            _ => None,
        }
    }

    /// GitCode 镜像仓只镜像官方源仓：仅 `AstralSightStudios/AstroBox-Repo` 前缀命中，
    /// 返回去掉 `https://raw.githubusercontent.com/` 后的路径（保留 `AstralSightStudios/AstroBox-Repo/` 段）；
    /// 其它仓库文件（第三方资源 manifest、固件等）不在镜像仓内，返回 `None` 保持 GitHub 直连。
    fn gitcode_repo_rest(url: &str) -> Option<&str> {
        if !url.starts_with(GITCODE_MIRRORED_REPO_PREFIX) {
            return None;
        }
        url.strip_prefix("https://raw.githubusercontent.com/")
    }

    pub fn convert_url(self, url: &str) -> String {
        if !is_convertible_github_url(url) {
            return url.to_owned();
        }

        // GitCode 镜像仓（Xuanwu/Jieyuan）：数据文件改走 GitCode API contents 接口
        // （raw.gitcode.com 对 >~30KB 的文件返回 403「暂不支持预览」；API 返回 base64
        // JSON，由拉取层 github_aware_bytes 解码）。镜像仓只镜像官方源仓，其它仓库
        // 文件不在镜像仓内，保持 GitHub 直连。
        if let Some(mirror) = self.gitcode_mirror() {
            return match Self::gitcode_repo_rest(url) {
                Some(rest) => format!(
                    "{}{}?format=raw&ref={}",
                    mirror.api_prefix, rest, mirror.branch
                ),
                None => url.to_owned(),
            };
        }

        if let Some((from, to)) = self.host_rewrite() {
            return url.replacen(from, to, 1);
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
        // GitCode 镜像仓的图片/二进制（通常为小文件）：保持 raw.gitcode.com host 替换
        // 直出原始内容（API 返回 base64 JSON，不适合 <img> 直接消费）。仅限官方源仓文件。
        if let Some(mirror) = self.gitcode_mirror() {
            return match Self::gitcode_repo_rest(url) {
                Some(rest) => format!("{}{}", mirror.raw_prefix, rest),
                None => url.to_owned(),
            };
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
    fn abox_mirror_rewrites_as_host_replace() {
        let url = "https://raw.githubusercontent.com/owner/repo/main/index.csv";
        assert_eq!(
            GitHubCdn::AboxMirror.convert_url(url),
            "https://mirror.abox.run/owner/repo/main/index.csv"
        );
        // 非 GitHub 地址不动；图片改写与普通改写一致。
        assert_eq!(
            GitHubCdn::AboxMirror.convert_url("https://example.com/file.bin"),
            "https://example.com/file.bin"
        );
        assert_eq!(
            GitHubCdn::AboxMirror.convert_asset_url(url),
            GitHubCdn::AboxMirror.convert_url(url)
        );
        // 数据获取与 Raw 一致：不走 AstroBox source-cdn 签名/内联。
        assert!(!GitHubCdn::AboxMirror.uses_astrobox_source_cdn());
    }

    #[test]
    fn xuanwu_uses_gitcode_api() {
        let url = "https://raw.githubusercontent.com/AstralSightStudios/AstroBox-Repo/refs/heads/main/index_v2.csv";
        // 数据文件改写为 GitCode API contents（raw.gitcode.com 对较大文件 403）。
        assert_eq!(
            GitHubCdn::Xuanwu.convert_url(url),
            "https://api.gitcode.com/api/v5/repos/gcw_MdSkpmRq/AstroBox-Repo-Mirror/contents/AstralSightStudios/AstroBox-Repo/refs/heads/main/index_v2.csv?format=raw&ref=main"
        );
        // 镜像仓只镜像官方源仓：第三方资源仓库文件保持 GitHub 直连。
        let third_party =
            "https://raw.githubusercontent.com/some/other-repo/main/manifest.json";
        assert_eq!(GitHubCdn::Xuanwu.convert_url(third_party), third_party);
        // gist / release / archive 无官方源仓前缀 → 同样保持直连。
        let gist = "https://gist.githubusercontent.com/user/gist/raw/file.txt";
        assert_eq!(GitHubCdn::Xuanwu.convert_url(gist), gist);
        let release =
            "https://github.com/some/repo/releases/download/v1.0/app.zip";
        assert_eq!(GitHubCdn::Xuanwu.convert_url(release), release);
        // 非 GitHub 地址不动。
        let ext = "https://example.com/file.bin";
        assert_eq!(GitHubCdn::Xuanwu.convert_url(ext), ext);
        // 图片/二进制走 raw.gitcode.com host 替换直出（API 返回 base64 JSON 不适合 <img>）。
        assert_eq!(
            GitHubCdn::Xuanwu.convert_asset_url(url),
            "https://raw.gitcode.com/gcw_MdSkpmRq/AstroBox-Repo-Mirror/raw/main/AstralSightStudios/AstroBox-Repo/refs/heads/main/index_v2.csv"
        );
        // 第三方图片保持直连。
        assert_eq!(GitHubCdn::Xuanwu.convert_asset_url(third_party), third_party);
        // 测速探测 URL（官方源仓）走 API 改写。
        assert_eq!(
            GitHubCdn::Xuanwu.probe_url(url),
            "https://api.gitcode.com/api/v5/repos/gcw_MdSkpmRq/AstroBox-Repo-Mirror/contents/AstralSightStudios/AstroBox-Repo/refs/heads/main/index_v2.csv?format=raw&ref=main"
        );
        // 数据获取与 Raw 一致：不走 AstroBox source-cdn 签名/内联。
        assert!(!GitHubCdn::Xuanwu.uses_astrobox_source_cdn());
    }

    #[test]
    fn jieyuan_uses_gitcode_api() {
        let url = "https://raw.githubusercontent.com/AstralSightStudios/AstroBox-Repo/refs/heads/main/index_v2.csv";
        assert_eq!(
            GitHubCdn::Jieyuan.convert_url(url),
            "https://api.gitcode.com/api/v5/repos/Bikboke/abmirror/contents/AstralSightStudios/AstroBox-Repo/refs/heads/main/index_v2.csv?format=raw&ref=main"
        );
        // 第三方资源仓库文件保持 GitHub 直连。
        let third_party =
            "https://raw.githubusercontent.com/some/other-repo/main/manifest.json";
        assert_eq!(GitHubCdn::Jieyuan.convert_url(third_party), third_party);
        // gist / release / archive 无官方源仓前缀 → 同样保持直连。
        let gist = "https://gist.githubusercontent.com/user/gist/raw/file.txt";
        assert_eq!(GitHubCdn::Jieyuan.convert_url(gist), gist);
        let release =
            "https://github.com/some/repo/releases/download/v1.0/app.zip";
        assert_eq!(GitHubCdn::Jieyuan.convert_url(release), release);
        // 非 GitHub 地址不动。
        let ext = "https://example.com/file.bin";
        assert_eq!(GitHubCdn::Jieyuan.convert_url(ext), ext);
        // 图片/二进制走 raw.gitcode.com host 替换直出。
        assert_eq!(
            GitHubCdn::Jieyuan.convert_asset_url(url),
            "https://raw.gitcode.com/Bikboke/abmirror/raw/main/AstralSightStudios/AstroBox-Repo/refs/heads/main/index_v2.csv"
        );
        assert_eq!(GitHubCdn::Jieyuan.convert_asset_url(third_party), third_party);
        // 测速探测 URL（官方源仓）走 API 改写。
        assert_eq!(
            GitHubCdn::Jieyuan.probe_url(url),
            "https://api.gitcode.com/api/v5/repos/Bikboke/abmirror/contents/AstralSightStudios/AstroBox-Repo/refs/heads/main/index_v2.csv?format=raw&ref=main"
        );
        // 数据获取与 Raw 一致：不走 AstroBox source-cdn 签名/内联。
        assert!(!GitHubCdn::Jieyuan.uses_astrobox_source_cdn());
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
