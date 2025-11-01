use std::sync::LazyLock;

use reqwest::{Client, ClientBuilder, NoProxy, Proxy};
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use sysproxy::Sysproxy;

pub static DEFAULT_CLIENT: LazyLock<Client> = LazyLock::new(build_client);

fn build_client() -> Client {
    default_client_builder().build().unwrap()
}

pub fn default_client() -> Client {
    DEFAULT_CLIENT.clone()
}

pub fn default_client_builder() -> ClientBuilder {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    if let Ok(proxy) = Sysproxy::get_system_proxy() {
        if proxy.enable {
            return Client::builder().danger_accept_invalid_certs(true).proxy(
                Proxy::all(format!("{}:{}", proxy.host, proxy.port))
                    .unwrap()
                    .no_proxy(NoProxy::from_string(&proxy.bypass.as_str())),
            );
        }
    }

    Client::builder()
}
