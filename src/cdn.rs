use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum GitHubCdn {
    Raw,
    AstroBoxProMirror,
    AstroBoxProMirrorWaterFlames,
    GhFast,
    GhProxy,
}

impl GitHubCdn {
    pub fn convert_url(self, url: &str) -> String {
        if !url.contains("https://raw.githubusercontent.com/") {
            return url.to_owned();
        }

        match self {
            GitHubCdn::Raw => url.to_owned(),
            GitHubCdn::AstroBoxProMirror => url.replace(
                "https://raw.githubusercontent.com/",
                "https://api.astrobox.online/ghoss/",
            ),
            GitHubCdn::AstroBoxProMirrorWaterFlames => url.replace(
                "https://raw.githubusercontent.com/",
                "https://api-astrobox.waterflames.cn/ghoss/",
            ),
            GitHubCdn::GhFast => format!(
                "https://ghfast.top/{}",
                url.strip_prefix("https://").unwrap_or(url)
            ),
            GitHubCdn::GhProxy => format!(
                "https://gh-proxy.com/{}",
                url.strip_prefix("https://").unwrap_or(url)
            ),
        }
    }
}
