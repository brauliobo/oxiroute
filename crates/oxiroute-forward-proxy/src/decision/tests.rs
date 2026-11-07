use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;

use super::*;
use crate::{AuthRequest, ForbiddenDestinationPolicy};

struct AllowAuth;

#[async_trait]
impl ProxyAuthenticator for AllowAuth {
    async fn authenticate(&self, _request: AuthRequest<'_>) -> Result<Principal, AuthError> {
        Ok(Principal::new("wire-test"))
    }
}

struct PanicAuth;

#[async_trait]
impl ProxyAuthenticator for PanicAuth {
    async fn authenticate(&self, _request: AuthRequest<'_>) -> Result<Principal, AuthError> {
        panic!("duplicate credentials must be rejected before authentication")
    }
}

struct PublicResolver;

#[async_trait]
impl Resolver for PublicResolver {
    async fn resolve(&self, _name: &str) -> Result<Vec<IpAddr>, ResolveError> {
        Ok(vec!["93.184.216.34".parse().unwrap()])
    }
}

struct RebindingResolver {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Resolver for RebindingResolver {
    async fn resolve(&self, _name: &str) -> Result<Vec<IpAddr>, ResolveError> {
        if self.calls.fetch_add(1, Ordering::Relaxed) == 0 {
            Ok(vec!["93.184.216.34".parse().unwrap()])
        } else {
            Ok(vec!["127.0.0.1".parse().unwrap()])
        }
    }
}

#[tokio::test]
async fn decision_contains_approved_addresses_but_no_credentials() {
    let proxy = ForwardProxy::new(AllowAuth, PublicResolver, ForbiddenDestinationPolicy);
    let mut headers = HeaderMap::new();
    headers.insert(
        header::PROXY_AUTHORIZATION,
        "Bearer do-not-retain".parse().unwrap(),
    );
    let decision = proxy
        .decide(IncomingRequest {
            protocol: Protocol::Http1,
            client_addr: "127.0.0.1:1234".parse().unwrap(),
            method: &Method::GET,
            target: "http://example.com/resource",
            headers: &headers,
        })
        .await
        .unwrap();
    let Decision::Forward(decision) = decision else {
        panic!("forward decision expected");
    };
    assert_eq!(decision.target, "/resource");
    assert!(!decision.headers.contains_key(header::PROXY_AUTHORIZATION));
    assert!(!format!("{decision:?}").contains("do-not-retain"));
    assert_eq!(decision.destination.socket_addresses().len(), 1);
}

#[tokio::test]
async fn dns_answer_is_checked_once_and_pinned_for_connection() {
    let calls = Arc::new(AtomicUsize::new(0));
    let proxy = ForwardProxy::new(
        AllowAuth,
        RebindingResolver {
            calls: Arc::clone(&calls),
        },
        ForbiddenDestinationPolicy,
    );
    let decision = proxy
        .decide(IncomingRequest {
            protocol: Protocol::Http1,
            client_addr: "127.0.0.1:1234".parse().unwrap(),
            method: &Method::GET,
            target: "http://example.com/resource",
            headers: &HeaderMap::new(),
        })
        .await
        .expect("first DNS answer is approved");
    let Decision::Forward(decision) = decision else {
        panic!("forward decision expected");
    };
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        decision.destination.socket_addresses(),
        &["93.184.216.34:80".parse().unwrap()]
    );
}

#[tokio::test]
async fn duplicate_proxy_authorization_is_rejected_before_authentication() {
    let proxy = ForwardProxy::new(PanicAuth, PublicResolver, ForbiddenDestinationPolicy);
    let mut headers = HeaderMap::new();
    headers.append(header::PROXY_AUTHORIZATION, "Bearer first".parse().unwrap());
    headers.append(
        header::PROXY_AUTHORIZATION,
        "Bearer second".parse().unwrap(),
    );

    let error = proxy
        .decide(IncomingRequest {
            protocol: Protocol::Http1,
            client_addr: "127.0.0.1:1234".parse().unwrap(),
            method: &Method::GET,
            target: "http://example.com/resource",
            headers: &headers,
        })
        .await
        .expect_err("duplicate proxy credentials must fail closed");

    assert!(matches!(
        error,
        DecisionError::Authentication(AuthError::Invalid)
    ));
    assert_eq!(
        error.rejection().status,
        StatusCode::PROXY_AUTHENTICATION_REQUIRED
    );
}

#[tokio::test]
async fn h3_connect_uses_the_shared_tunnel_decision_path() {
    let proxy = ForwardProxy::new(AllowAuth, PublicResolver, ForbiddenDestinationPolicy);
    let result = proxy
        .decide(IncomingRequest {
            protocol: Protocol::Http3,
            client_addr: "127.0.0.1:1234".parse().unwrap(),
            method: &Method::CONNECT,
            target: "example.com:443",
            headers: &HeaderMap::new(),
        })
        .await
        .expect("H3 CONNECT decision");
    assert!(matches!(result, Decision::Tunnel(_)));
}
