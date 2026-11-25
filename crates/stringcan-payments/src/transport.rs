use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::ProcessorError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: Method,
    /// Path and nothing else, e.g. `/v1/accounts`; the host is the transport's business.
    pub path: String,
    pub form: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
}

impl HttpRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn form_value(&self, name: &str) -> Option<&str> {
        self.form
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

/// The only seam to the network. The transport owns credentials, so a recorded
/// request never contains the API key.
#[async_trait]
pub trait Transport: Send + Sync {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, ProcessorError>;
}

#[async_trait]
impl<T: Transport + ?Sized> Transport for std::sync::Arc<T> {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, ProcessorError> {
        (**self).send(req).await
    }
}

/// Serves queued responses and records what was sent. Used by every test.
#[derive(Default)]
pub struct FixtureTransport {
    responses: Mutex<VecDeque<HttpResponse>>,
    sent: Mutex<Vec<HttpRequest>>,
}

impl FixtureTransport {
    pub fn new(responses: impl IntoIterator<Item = HttpResponse>) -> Self {
        FixtureTransport {
            responses: Mutex::new(responses.into_iter().collect()),
            sent: Mutex::default(),
        }
    }

    pub fn ok(body: &str) -> HttpResponse {
        HttpResponse {
            status: 200,
            body: body.to_owned(),
        }
    }

    pub fn status(status: u16, body: &str) -> HttpResponse {
        HttpResponse {
            status,
            body: body.to_owned(),
        }
    }

    pub fn requests(&self) -> Vec<HttpRequest> {
        self.sent.lock().expect("fixture lock").clone()
    }
}

#[async_trait]
impl Transport for FixtureTransport {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, ProcessorError> {
        self.sent.lock().expect("fixture lock").push(req);
        self.responses
            .lock()
            .expect("fixture lock")
            .pop_front()
            .ok_or(ProcessorError::Invalid("no fixture response queued"))
    }
}

#[cfg(feature = "http")]
mod live {
    use std::time::Duration;

    use super::{async_trait, HttpRequest, HttpResponse, Method, ProcessorError, Transport};
    use crate::StripeConfig;

    const BASE: &str = "https://api.stripe.com";

    /// Talks to api.stripe.com with a test-mode key (enforced by `StripeConfig`).
    /// Nothing in the test suite constructs this.
    pub struct ReqwestTransport {
        client: reqwest::Client,
        key: StripeConfig,
    }

    impl ReqwestTransport {
        pub fn new(key: StripeConfig) -> Result<Self, ProcessorError> {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .map_err(|_| ProcessorError::Invalid("http client"))?;
            Ok(ReqwestTransport { client, key })
        }
    }

    #[async_trait]
    impl Transport for ReqwestTransport {
        async fn send(&self, req: HttpRequest) -> Result<HttpResponse, ProcessorError> {
            let url = format!("{BASE}{}", req.path);
            let mut b = match req.method {
                Method::Get => self.client.get(url),
                Method::Post => self.client.post(url).form(&req.form),
            }
            .bearer_auth(self.key.secret());
            for (k, v) in &req.headers {
                b = b.header(k, v);
            }
            let resp = b
                .send()
                .await
                .map_err(|e| ProcessorError::Transient(e.without_url().to_string()))?;
            let status = resp.status().as_u16();
            let body = resp
                .text()
                .await
                .map_err(|e| ProcessorError::Transient(e.without_url().to_string()))?;
            Ok(HttpResponse { status, body })
        }
    }
}

#[cfg(feature = "http")]
pub use live::ReqwestTransport;
