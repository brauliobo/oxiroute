#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

impl HttpRequest {
    pub(super) fn new(method: &str, url: &str, body: Vec<u8>) -> Self {
        Self {
            method: method.into(),
            url: url.into(),
            headers: BTreeMap::new(),
            body,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    #[must_use]
    pub fn new(status: u16, url: impl Into<String>, body: Vec<u8>) -> Self {
        Self {
            status,
            url: url.into(),
            headers: BTreeMap::new(),
            body,
        }
    }

    #[must_use]
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.insert(name.to_ascii_lowercase(), value.into());
        self
    }

    pub(super) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

pub trait AcmeTransport: Send + Sync {
    /// Sends one bounded ACME request without following redirects.
    ///
    /// # Errors
    ///
    /// Returns a transport error when the injected transport cannot produce a response.
    fn request(&self, request: HttpRequest) -> Result<HttpResponse, TransportError>;

    /// Sends one request under a caller-owned cancellation operation.
    ///
    /// # Errors
    ///
    /// Returns [`AcmeError::Cancelled`] when cancellation or the operation deadline wins, or the
    /// same redacted transport error as [`Self::request`] otherwise.
    fn request_with_operation(
        &self,
        request: HttpRequest,
        operation: &AcmeOperation,
    ) -> Result<HttpResponse, AcmeError> {
        operation.check()?;
        let response = self.request(request).map_err(AcmeError::Transport)?;
        operation.check()?;
        Ok(response)
    }
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
#[error("ACME transport request failed")]
pub struct TransportError;
