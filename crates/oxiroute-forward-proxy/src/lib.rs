//! Protocol and security primitives for an explicit forward proxy.
//!
//! This crate deliberately does not open listeners or connect sockets. A runtime must authenticate,
//! resolve, authorize, and then connect one of the exact socket addresses in [`ApprovedDestination`].

mod auth;
mod decision;
mod headers;
mod policy;
mod target;
mod tunnel;

pub use auth::{
    AuthContext, AuthError, AuthRequest, Principal, ProxyAuthenticator, ProxyCredentials,
};
pub use decision::{
    Decision, DecisionError, DecisionRejection, ForwardDecision, ForwardProxy, H3RequestError,
    IncomingRequest, Protocol, TunnelDecision,
};
pub use headers::{HeaderSanitizationError, sanitize_request_headers};
pub use policy::{
    ApprovedDestination, DestinationPolicy, DestinationRules, ForbiddenDestinationPolicy,
    PolicyContext, PolicyError, ResolveError, Resolver, RuleError,
};
pub use target::{
    Destination, ForwardScheme, ForwardTarget, Host, TargetError, parse_absolute_form,
    parse_connect_authority,
};
pub use tunnel::{
    BoundedTunnel, OverreadIo, TunnelConfigError, TunnelEnd, TunnelLimits, TunnelOutcome,
    TunnelStats,
};

/// ALPN identifier required by RFC 9114 HTTP/3 connections.
pub const H3_ALPN: &[u8] = b"h3";
