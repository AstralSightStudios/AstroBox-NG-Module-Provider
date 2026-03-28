use reqwest::{Client, ClientBuilder};

pub fn default_client() -> Client {
    netcfg::default_client()
}

pub fn default_client_builder() -> ClientBuilder {
    netcfg::default_client_builder()
}
