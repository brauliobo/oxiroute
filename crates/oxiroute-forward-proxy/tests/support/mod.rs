use std::net::IpAddr;

use async_trait::async_trait;
use oxiroute_forward_proxy::{
    AuthError, AuthRequest, Decision, DecisionError, ForbiddenDestinationPolicy, ForwardProxy,
    Principal, Protocol, ProxyAuthenticator, ResolveError, Resolver,
};

pub struct WireAuthenticator;

#[async_trait]
impl ProxyAuthenticator for WireAuthenticator {
    async fn authenticate(&self, request: AuthRequest<'_>) -> Result<Principal, AuthError> {
        match request
            .credentials
            .map(oxiroute_forward_proxy::ProxyCredentials::expose)
        {
            Some(b"Bearer wire-test") => Ok(Principal::new("wire-client")),
            Some(_) => Err(AuthError::Invalid),
            None => Err(AuthError::Missing),
        }
    }
}

pub struct PublicResolver;

#[async_trait]
impl Resolver for PublicResolver {
    async fn resolve(&self, name: &str) -> Result<Vec<IpAddr>, ResolveError> {
        match name {
            "example.com" => Ok(vec!["93.184.216.34".parse().expect("public fixture IP")]),
            _ => Err(ResolveError::NoAddresses),
        }
    }
}

pub fn proxy() -> ForwardProxy<WireAuthenticator, PublicResolver, ForbiddenDestinationPolicy> {
    ForwardProxy::new(
        WireAuthenticator,
        PublicResolver,
        ForbiddenDestinationPolicy,
    )
}

pub async fn decide<B>(
    protocol: Protocol,
    client_addr: std::net::SocketAddr,
    request: &http::Request<B>,
) -> Result<Decision, DecisionError> {
    proxy().decide_request(protocol, client_addr, request).await
}
