use std::{
    collections::{HashSet, VecDeque},
    convert::Infallible,
    error::Error,
    future::{Future as _, poll_fn},
    io,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use bytes::{Buf as _, Bytes, BytesMut};
use hickory_resolver::{
    TokioResolver,
    config::{LookupIpStrategy, NameServerConfig, ResolverConfig, ResolverOpts},
    net::runtime::TokioRuntimeProvider,
};
use http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode, Uri, header};
use http_body_util::{BodyExt as _, Full, Limited, combinators::BoxBody};
use hyper::body::{Body, Frame, Incoming};
use hyper_util::rt::TokioIo;
use openssl::{
    sha::Sha256,
    ssl::{SslConnector, SslMethod, SslVerifyMode},
};
use oxiroute_acme::ChallengeStore;
use oxiroute_cache::{CachedResponse, ResponseTiming};
use oxiroute_config::{
    ForwardAccessAction, ForwardAccessCondition, ForwardAccessMatcher, ForwardAccessPolicy,
    ForwardAuditMode, ForwardDirectFallback, ForwardHeaderPolicy, ForwardHttpVersion, ForwardPeer,
    ForwardProxyAuth, ForwardResolverPolicy, ForwardTimeRange, ForwardViaPolicy, ForwardWeekday,
    ForwardedForPolicy,
};
use oxiroute_forward_proxy::{
    ApprovedDestination, BoundedTunnel, Destination, DestinationRules, ForwardScheme,
    H2TunnelStream, Host, PolicyContext, Principal, Protocol, RuleError, TimeWindow, TunnelLimits,
    parse_absolute_form, parse_connect_authority, parse_connect_udp_path, sanitize_request_headers,
};
use pingora::{
    apps::{HttpServerApp, HttpServerOptions, ReusedHttpStream},
    protocols::http::{
        ServerSession,
        v2::server::{H2Options, default_h2_options},
    },
    server::ShutdownWatch,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpStream, UdpSocket},
    sync::{Mutex, OwnedSemaphorePermit, Semaphore, mpsc, oneshot},
    time::{Instant, Sleep, timeout_at},
};
use tokio_openssl::SslStream;

use crate::{
    H3UpstreamError, H3UpstreamPlan,
    capsule::relay_udp,
    http::action::BasicHtpasswdAccess,
    http::cache::{
        CacheBackendError, CacheFailureClass, CacheRequest, CacheStart, CacheStartFailure,
        CacheTransaction, HttpCachePlan,
    },
    secure_bearer::{HeaderCardinality, SecureBearerToken, single_header},
};

mod access;
mod app;
mod blueprint;
mod body;
mod cache;
mod connect;
mod h3_forward;
mod handle;
mod headers;
mod masque;
mod plan;
mod service;

pub use app::*;
pub(crate) use blueprint::*;
#[allow(clippy::wildcard_imports)]
use body::*;
#[allow(clippy::wildcard_imports)]
use cache::*;
#[allow(clippy::wildcard_imports)]
use connect::*;
#[allow(clippy::wildcard_imports)]
use h3_forward::*;
#[allow(clippy::wildcard_imports)]
use headers::*;
#[allow(clippy::wildcard_imports)]
use masque::*;
pub use plan::*;

#[cfg(test)]
mod tests;
