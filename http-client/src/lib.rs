///!
///! # http-client-provider
///! This library exposes the HTTP client capability to waSCC-compliant actors
mod http_client;

#[macro_use]
extern crate wascc_codec as codec;

#[macro_use]
extern crate log;

use codec::capabilities::{
    CapabilityDescriptor, CapabilityProvider, Dispatcher, NullDispatcher, OperationDirection,
    OP_GET_CAPABILITY_DESCRIPTOR,
};
use codec::core::{CapabilityConfiguration, OP_BIND_ACTOR, OP_REMOVE_ACTOR};
use codec::http::{Request, OP_PERFORM_REQUEST};
use codec::{deserialize, serialize, SYSTEM_ACTOR};
use std::collections::HashMap;
use std::error::Error;
use std::sync::{Arc, RwLock};
use std::time::Duration;

const CAPABILITY_ID: &str = "wascc:http_client";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const REVISION: u32 = 0;

#[cfg(not(feature = "static_plugin"))]
capability_provider!(HttpClientProvider, HttpClientProvider::new);

/// An implementation HTTP client provider using reqwest.
pub struct HttpClientProvider {
    dispatcher: Arc<RwLock<Box<dyn Dispatcher>>>,
    clients: Arc<RwLock<HashMap<String, reqwest::Client>>>,
    runtime: tokio::runtime::Runtime,
}

impl HttpClientProvider {
    /// Create a new HTTP client provider.
    pub fn new() -> Self {
        Self::default()
    }

    /// Configure the HTTP client for a particular actor.
    /// Each actor gets a dedicated client so that we can take advantage of connection pooling.
    /// TODO: This needs to set things like timeouts, redirects, etc.
    fn configure(&self, config: CapabilityConfiguration) -> Result<Vec<u8>, Box<dyn Error>> {
        let timeout = match config.values.get("timeout") {
            Some(v) => {
                let parsed: u64 = v.parse()?;
                Duration::new(parsed, 0)
            }
            None => Duration::new(30, 0),
        };

        let redirect_policy = match config.values.get("max_redirects") {
            Some(v) => {
                let parsed: usize = v.parse()?;
                reqwest::redirect::Policy::limited(parsed)
            }
            None => reqwest::redirect::Policy::default(),
        };

        self.clients.write().unwrap().insert(
            config.module.clone(),
            reqwest::Client::builder()
                .timeout(timeout)
                .redirect(redirect_policy)
                .build()?,
        );
        Ok(vec![])
    }

    /// Clean up resources when a actor disconnects.
    /// This removes the HTTP client associated with an actor.
    fn deconfigure(&self, config: CapabilityConfiguration) -> Result<Vec<u8>, Box<dyn Error>> {
        if self
            .clients
            .write()
            .unwrap()
            .remove(&config.module)
            .is_none()
        {
            warn!(
                "attempted to remove non-existent actor: {}",
                config.module.as_str()
            );
        }

        Ok(vec![])
    }

    /// Make a HTTP request.
    fn request(&self, actor: &str, msg: Request) -> Result<Vec<u8>, Box<dyn Error>> {
        let lock = self.clients.read().unwrap();
        let client = lock.get(actor).unwrap();
        self.runtime
            .handle()
            .block_on(async { http_client::request(&client, msg).await })
    }

    fn get_descriptor(&self) -> Result<Vec<u8>, Box<dyn Error>> {
        use OperationDirection::ToProvider;
        Ok(serialize(
            CapabilityDescriptor::builder()
                .id(CAPABILITY_ID)
                .name("wasCC HTTP Client Provider")
                .long_description("A http client provider")
                .version(VERSION)
                .revision(REVISION)
                .with_operation(OP_PERFORM_REQUEST, ToProvider, "Perform a http request")
                .build(),
        )?)
    }
}

impl Default for HttpClientProvider {
    fn default() -> Self {
        let _ = env_logger::builder().format_module_path(false).try_init();

        let r = tokio::runtime::Builder::new()
            .threaded_scheduler()
            .enable_all()
            .build()
            .unwrap();

        HttpClientProvider {
            dispatcher: Arc::new(RwLock::new(Box::new(NullDispatcher::new()))),
            clients: Arc::new(RwLock::new(HashMap::new())),
            runtime: r,
        }
    }
}

/// Implements the CapabilityProvider interface.
impl CapabilityProvider for HttpClientProvider {
    fn configure_dispatch(&self, dispatcher: Box<dyn Dispatcher>) -> Result<(), Box<dyn Error>> {
        info!("Dispatcher configured");

        let mut lock = self.dispatcher.write().unwrap();
        *lock = dispatcher;
        Ok(())
    }

    /// Handle all calls from actors.
    fn handle_call(&self, actor: &str, op: &str, msg: &[u8]) -> Result<Vec<u8>, Box<dyn Error>> {
        match op {
            OP_BIND_ACTOR if actor == SYSTEM_ACTOR => self.configure(deserialize(msg)?),
            OP_REMOVE_ACTOR if actor == SYSTEM_ACTOR => self.deconfigure(deserialize(msg)?),
            OP_PERFORM_REQUEST => self.request(actor, deserialize(msg)?),
            OP_GET_CAPABILITY_DESCRIPTOR => self.get_descriptor(),
            _ => Err(format!("Unknown operation: {}", op).into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codec::deserialize;
    use codec::http::Response;
    use mockito::mock;

    #[test]
    fn test_request() {
        let _ = env_logger::try_init();
        let request = Request {
            method: "GET".to_string(),
            path: mockito::server_url(),
            header: HashMap::new(),
            body: vec![],
            query_string: String::new(),
        };

        let _m = mock("GET", "/")
            .with_header("content-type", "text/plain")
            .with_body("ohai")
            .create();

        let hp = HttpClientProvider::new();
        hp.configure(CapabilityConfiguration {
            module: "test".to_string(),
            values: HashMap::new(),
        })
        .unwrap();

        let result = hp.request("test", request).unwrap();
        let response: Response = deserialize(result.as_slice()).unwrap();

        assert_eq!(response.status_code, 200);
    }
}
