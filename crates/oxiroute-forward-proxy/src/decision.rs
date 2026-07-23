use std::net::{IpAddr, SocketAddr};

use http::{HeaderMap, Method, Request, StatusCode, Uri, header};

use crate::{
    ApprovedDestination, AuthContext, AuthError, AuthRequest, DestinationPolicy,
    HeaderSanitizationError, Host, PolicyContext, PolicyError, Principal, ProxyAuthenticator,
    ProxyCredentials, ResolveError, Resolver, TargetError, parse_absolute_form,
    parse_connect_authority, sanitize_request_headers,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Protocol {
    Http1,
    Http2,
    Http3,
}

pub struct IncomingRequest<'a> {
    pub protocol: Protocol,
    pub client_addr: SocketAddr,
    pub method: &'a Method,
    /// H1 raw request-target, or the URI reconstructed from H2/H3 pseudo-headers.
    pub target: &'a str,
    pub headers: &'a HeaderMap,
}

#[derive(Clone, Debug)]
pub struct ForwardDecision {
    pub principal: Principal,
    pub method: Method,
    pub scheme: crate::ForwardScheme,
    pub target: Uri,
    pub headers: HeaderMap,
    pub destination: ApprovedDestination,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TunnelDecision {
    pub principal: Principal,
    pub destination: ApprovedDestination,
}

#[derive(Clone, Debug)]
pub enum Decision {
    Forward(Box<ForwardDecision>),
    Tunnel(TunnelDecision),
}

#[derive(Debug, thiserror::Error)]
pub enum DecisionError {
    #[error(transparent)]
    InvalidTarget(#[from] TargetError),
    #[error(transparent)]
    Authentication(#[from] AuthError),
    #[error(transparent)]
    Resolution(#[from] ResolveError),
    #[error(transparent)]
    Policy(#[from] PolicyError),
    #[error(transparent)]
    InvalidHeaders(#[from] HeaderSanitizationError),
    #[error(transparent)]
    InvalidHttp3(#[from] H3RequestError),
}

impl DecisionError {
    /// Maps a fail-closed decision error to a protocol-neutral HTTP rejection.
    #[must_use]
    pub const fn rejection(&self) -> DecisionRejection {
        match self {
            Self::InvalidTarget(_) | Self::InvalidHeaders(_) | Self::InvalidHttp3(_) => {
                DecisionRejection {
                    status: StatusCode::BAD_REQUEST,
                    proxy_authenticate: false,
                }
            }
            Self::Authentication(_) => DecisionRejection {
                status: StatusCode::PROXY_AUTHENTICATION_REQUIRED,
                proxy_authenticate: true,
            },
            Self::Policy(_) => DecisionRejection {
                status: StatusCode::FORBIDDEN,
                proxy_authenticate: false,
            },
            Self::Resolution(_) => DecisionRejection {
                status: StatusCode::BAD_GATEWAY,
                proxy_authenticate: false,
            },
        }
    }
}

/// Safe HTTP response metadata for a rejected request. It never carries credentials or details.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecisionRejection {
    pub status: StatusCode,
    pub proxy_authenticate: bool,
}

/// Malformed or unsupported HTTP/3 pseudo-header shape detected after QPACK decoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum H3RequestError {
    #[error("classic CONNECT requires only :method and :authority pseudo-headers")]
    MalformedClassicConnect,
    #[error("extended CONNECT is not a classic TCP tunnel")]
    ExtendedConnectUnsupported,
}

pub struct ForwardProxy<A, R, P> {
    authenticator: A,
    resolver: R,
    policy: P,
}

impl<A, R, P> ForwardProxy<A, R, P>
where
    A: ProxyAuthenticator,
    R: Resolver,
    P: DestinationPolicy,
{
    #[must_use]
    pub fn new(authenticator: A, resolver: R, policy: P) -> Self {
        Self {
            authenticator,
            resolver,
            policy,
        }
    }

    /// Extracts and validates a framed HTTP request before running the shared decision pipeline.
    ///
    /// HTTP/3 classic CONNECT is required to contain an authority but no scheme, path, or
    /// `:protocol`. The request stream should be reset with `H3_MESSAGE_ERROR` when this returns
    /// [`DecisionError::InvalidHttp3`].
    ///
    /// # Errors
    ///
    /// Returns [`DecisionError`] for malformed protocol metadata or any normal decision failure.
    pub async fn decide_request<B>(
        &self,
        protocol: Protocol,
        client_addr: SocketAddr,
        request: &Request<B>,
    ) -> Result<Decision, DecisionError> {
        if protocol == Protocol::Http3 && *request.method() == Method::CONNECT {
            if request.extensions().get::<h3::ext::Protocol>().is_some() {
                return Err(H3RequestError::ExtendedConnectUnsupported.into());
            }
            if request.uri().authority().is_none()
                || request.uri().scheme().is_some()
                || request.uri().path_and_query().is_some()
            {
                return Err(H3RequestError::MalformedClassicConnect.into());
            }
        }
        let target = request.uri().to_string();
        self.decide(IncomingRequest {
            protocol,
            client_addr,
            method: request.method(),
            target: &target,
            headers: request.headers(),
        })
        .await
    }

    /// Authenticates, resolves, authorizes, and converts an inbound request into an executable plan.
    ///
    /// # Errors
    ///
    /// Returns [`DecisionError`] if any fail-closed parsing, authentication, resolution, policy,
    /// sanitization, or protocol-shape validation fails.
    pub async fn decide(&self, request: IncomingRequest<'_>) -> Result<Decision, DecisionError> {
        let parsed = if request.method == Method::CONNECT {
            ParsedRequest::Tunnel(parse_connect_authority(request.target)?)
        } else {
            ParsedRequest::Forward(parse_absolute_form(request.target)?)
        };

        let credentials = request
            .headers
            .get(header::PROXY_AUTHORIZATION)
            .map(|value| ProxyCredentials::new(value.as_bytes()));
        let principal = self
            .authenticator
            .authenticate(AuthRequest {
                context: AuthContext {
                    protocol: request.protocol,
                    client_addr: request.client_addr,
                },
                credentials,
            })
            .await?;
        let destination = match &parsed {
            ParsedRequest::Forward(target) => &target.destination,
            ParsedRequest::Tunnel(destination) => destination,
        };
        let addresses = match &destination.host {
            Host::Ip(address) => vec![*address],
            Host::Dns(name) => self.resolver.resolve(name).await?,
        };
        if addresses.is_empty() {
            return Err(ResolveError::NoAddresses.into());
        }
        let addresses = deduplicate(addresses);
        self.policy.authorize(
            &PolicyContext {
                protocol: request.protocol,
                principal: principal.clone(),
            },
            destination,
            &addresses,
        )?;
        let approved = ApprovedDestination::new(destination.clone(), addresses);

        Ok(match parsed {
            ParsedRequest::Forward(target) => Decision::Forward(Box::new(ForwardDecision {
                principal,
                method: request.method.clone(),
                scheme: target.scheme,
                target: target.origin_form,
                headers: sanitize_request_headers(request.headers, &target.destination)?,
                destination: approved,
            })),
            ParsedRequest::Tunnel(_) => Decision::Tunnel(TunnelDecision {
                principal,
                destination: approved,
            }),
        })
    }
}

enum ParsedRequest {
    Forward(crate::ForwardTarget),
    Tunnel(crate::Destination),
}

fn deduplicate(addresses: Vec<IpAddr>) -> Vec<IpAddr> {
    let mut unique = Vec::with_capacity(addresses.len());
    for address in addresses {
        if !unique.contains(&address) {
            unique.push(address);
        }
    }
    unique
}

#[cfg(test)]
mod tests {
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

    struct PublicResolver;

    #[async_trait]
    impl Resolver for PublicResolver {
        async fn resolve(&self, _name: &str) -> Result<Vec<IpAddr>, ResolveError> {
            Ok(vec!["93.184.216.34".parse().unwrap()])
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
        assert_eq!(decision.destination.socket_addresses.len(), 1);
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
}
