use hostio::http::{Client, ClientBuilder};

pub fn default_client() -> Client {
    Client::new()
}

pub fn default_client_builder() -> ClientBuilder {
    Client::builder()
}
