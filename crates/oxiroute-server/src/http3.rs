use std::{
    error::Error,
    io,
    net::SocketAddr,
    sync::Arc,
    thread::{self, JoinHandle},
    time::Duration,
};

use bytes::{Buf as _, Bytes};
use h3::{
    quic::{RecvStream as _, SendStream as _},
    server::RequestResolver,
};
use http::{
    HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, Uri,
    header::{CONTENT_LENGTH, HOST},
    uri::Authority,
};
use log::{error, warn};
use oxiroute_cache::{CachedResponse, ResponseTiming};
use oxiroute_config::{
    DownstreamTimeoutPolicy, HttpRetryTarget, HttpRetryTrigger, is_unambiguous_http_path,
};
use pingora::apps::AcceptGateParticipant;
use quinn::crypto::rustls::QuicServerConfig;
use quinn::{Endpoint, ServerConfig, TransportConfig, VarInt};
use rustls::server::{ClientHello, ResolvesServerCert};
use tokio::{
    runtime::Builder,
    sync::{OwnedSemaphorePermit, Semaphore, watch},
    time::{Instant, timeout_at},
};

use crate::{
    AdmissionError, ForwardHttp1ServicePlan, H3UpstreamError, HealthFailure, HttpOperationResult,
    HttpServicePlan, ListenerMetrics, ListenerReservation, ListenerRuntime, ListenerRuntimeState,
    RuntimeGeneration, RuntimeMode, RuntimeReferenceKind, TlsProfilePlan,
    http::action::{
        HttpActionPlan, ProxyActionPlan, ProxyPolicyPlan, RedirectLocationPlan, StaticErrorTarget,
        StaticFile, StaticRequestDecision, StaticServeError, StaticTarget,
    },
    http::cache::{
        CacheFailureClass, CacheRequest, CacheStart, CacheStartFailure, CacheTransaction,
        HttpCachePlan,
    },
    http::hop_by_hop::hop_by_hop_removals,
    http::policy::{
        RedirectContext, RequestHeaderDecision, RequestPolicyContext, RequestPolicyError,
        decide_request_header, expand_redirect_location, normalized_redirect_host,
    },
    http::proxy::{
        apply_response_policy_map, cache_request_bypasses_cache, rewrite_upstream_path,
        selected_upstream_host,
    },
};

mod cache;
mod capability;
mod connection;
mod headers;
mod reverse;
mod runtime;
mod static_files;
mod upstream;
pub(crate) mod upstream_plan;

#[allow(clippy::wildcard_imports)]
use cache::*;
pub(crate) use capability::*;
#[allow(clippy::wildcard_imports)]
use connection::*;
pub(crate) use headers::*;
#[allow(clippy::wildcard_imports)]
use reverse::*;
pub use runtime::*;
#[allow(clippy::wildcard_imports)]
use static_files::*;
#[allow(clippy::wildcard_imports)]
use upstream::*;

#[cfg(test)]
mod tests;
