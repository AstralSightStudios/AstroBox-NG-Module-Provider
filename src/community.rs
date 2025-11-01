use async_trait::async_trait;
use std::sync::{Arc, Mutex, OnceLock};

pub mod models;
pub mod officialv2;

pub static COMMUNITY_PROVIDERS: OnceLock<Mutex<Vec<Arc<dyn CommunityProvider>>>> = OnceLock::new();

pub async fn add_community_provider(provider: Arc<dyn CommunityProvider>) {
    let providers = COMMUNITY_PROVIDERS.get_or_init(|| Mutex::new(Vec::new()));
    let mut locked = providers.lock().unwrap();
    locked.push(provider);
}

pub async fn remove_community_provider(name: &str) {
    let providers = COMMUNITY_PROVIDERS.get_or_init(|| Mutex::new(Vec::new()));
    let mut locked = providers.lock().unwrap();
    locked.retain(|p| p.provider_name() != name);
}

pub async fn get_community_provider(name: &str) -> Option<Arc<dyn CommunityProvider>> {
    let providers = COMMUNITY_PROVIDERS.get_or_init(|| Mutex::new(Vec::new()));
    let locked = providers.lock().unwrap();
    for p in locked.iter() {
        if p.provider_name() == name {
            return Some(Arc::clone(p));
        }
    }
    None
}

pub async fn list_community_providers() -> Vec<String> {
    let providers = COMMUNITY_PROVIDERS.get_or_init(|| Mutex::new(Vec::new()));
    let locked = providers.lock().unwrap();
    locked.iter().map(|p| p.provider_name()).collect()
}

#[async_trait]
pub trait CommunityProvider: Send + Sync {
    fn provider_name(&self) -> String;
    fn as_any(&self) -> &dyn std::any::Any;

    async fn refresh(&self) -> anyhow::Result<()>;

    fn state(&self) -> models::common::ProviderState;

    async fn get_page(
        &self,
        page: u32,
        limit: u32,
        search: models::common::SearchConfig,
    ) -> anyhow::Result<Vec<models::common::ManifestV2>>;
    async fn get_categories(&self) -> anyhow::Result<Vec<String>>;
    async fn get_item_manifest(
        &self,
        item_id: String,
    ) -> anyhow::Result<models::common::ManifestV2>;
    async fn download(
        &self,
        item_id: String,
        device: String,
        progress_cb: Option<Box<dyn Fn(models::common::ProgressData) + Send>>,
    ) -> anyhow::Result<std::path::PathBuf>;
    async fn get_total_items(&self) -> anyhow::Result<u64>;
}
