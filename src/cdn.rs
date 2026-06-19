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
    pub const ALL: [Self; 4] = [
        GitHubCdn::Raw,
        GitHubCdn::AstroBoxProMirrorWaterFlames,
        GitHubCdn::GhFast,
        GitHubCdn::GhProxy,
    ];

    pub fn normalized(self) -> Self {
        match self {
            GitHubCdn::AstroBoxProMirror => GitHubCdn::AstroBoxProMirrorWaterFlames,
            other => other,
        }
    }

    pub fn convert_url(self, url: &str) -> String {
        if !url.contains("https://raw.githubusercontent.com/") {
            return url.to_owned();
        }

        match self.normalized() {
            GitHubCdn::Raw => url.to_owned(),
            GitHubCdn::AstroBoxProMirror |
            GitHubCdn::AstroBoxProMirrorWaterFlames => url.to_owned(),
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
        Self::ALL
            .iter()
            .map(|item| serde_json::to_string(item).unwrap().replace('"', ""))
            .collect()
    }
}
