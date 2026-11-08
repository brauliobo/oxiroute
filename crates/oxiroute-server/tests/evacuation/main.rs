//! Evacuation: real proxy, real pools and localhost origins that the test releases explicitly.

#[path = "../support/config.rs"]
mod config_support;
#[path = "../support/http.rs"]
mod http_support;

use std::{
    net::SocketAddr,
    os::fd::AsRawFd as _,
    path::PathBuf,
    sync::{Arc, Barrier},
    time::Duration,
};

use oxiroute_config::{
    AccessLogPolicy, ConfigDraft, DnsResolutionPolicy, DownstreamTimeoutPolicy, HttpPathSelector,
    HttpProxyPolicy, HttpRetryBodySafety, HttpRetryBudget, HttpRetryMethodSafety, HttpRetryPolicy,
    HttpRoute, HttpRouteAction, HttpRoutePolicy, HttpService, HttpVersionPolicy, Listener,
    PoolResilience, Protocol, UpstreamAlgorithm, UpstreamConnectionReuse, UpstreamPool,
    UpstreamServer,
};
use oxiroute_server::{
    AdministrativeState, EvacuationReport, HttpDownstreamPolicyApp, HttpReverseProxy, LeftRunning,
    MAX_HTTP_ATTEMPTS, MonitoredHttpApp, RoundRobinPool, RuntimeMetrics, ServiceKind,
};
use pingora::{
    apps::ServerApp,
    protocols::{GetSocketDigest as _, SocketDigest},
    proxy::http_proxy,
    server::configuration::ServerConf,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::{Semaphore, mpsc, watch},
    task::JoinHandle,
    time::{sleep, timeout},
};

use config_support::{empty_config, socket_bind, socket_endpoint};
use http_support::{HttpResponse, http_request};

const TEST_TIMEOUT: Duration = Duration::from_secs(20);
const BODY: &str = "payload-123";

mod farm;
mod left_running;
mod limits;
mod moving;
mod origin;
mod pool;

use farm::*;
use origin::*;
