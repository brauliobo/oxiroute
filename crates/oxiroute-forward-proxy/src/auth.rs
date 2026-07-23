use std::{net::SocketAddr, sync::Arc};

use async_trait::async_trait;

use crate::Protocol;

/// A borrowed credential value. It intentionally does not implement `Debug` or `Display`.
#[derive(Clone, Copy)]
pub struct ProxyCredentials<'a>(&'a [u8]);

impl<'a> ProxyCredentials<'a> {
    #[must_use]
    pub fn new(value: &'a [u8]) -> Self {
        Self(value)
    }

    /// Exposes credentials only to an authentication provider.
    #[must_use]
    pub fn expose(self) -> &'a [u8] {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Principal {
    id: Arc<str>,
}

impl Principal {
    #[must_use]
    pub fn new(id: impl Into<Arc<str>>) -> Self {
        Self { id: id.into() }
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthContext {
    pub protocol: Protocol,
    pub client_addr: SocketAddr,
}

pub struct AuthRequest<'a> {
    pub context: AuthContext,
    pub credentials: Option<ProxyCredentials<'a>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AuthError {
    #[error("proxy credentials are required")]
    Missing,
    #[error("proxy credentials were rejected")]
    Invalid,
    #[error("proxy authentication is unavailable")]
    Unavailable,
}

/// Authenticates externally managed credentials and returns a non-secret identity.
///
/// Implementations own secret retrieval and comparison. The proxy core never stores credentials in
/// a decision or error and provides no inline username/password implementation.
#[async_trait]
pub trait ProxyAuthenticator: Send + Sync {
    async fn authenticate(&self, request: AuthRequest<'_>) -> Result<Principal, AuthError>;
}
