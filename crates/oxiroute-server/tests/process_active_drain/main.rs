#![allow(dead_code, unused_imports, clippy::duplicate_mod)]

#[path = "../support/config.rs"]
mod config_support;
#[path = "../support/fixtures.rs"]
mod fixture_support;
#[path = "../support/http.rs"]
mod http_support;
#[path = "../support/process.rs"]
mod process_support;
#[path = "../support/rtmp.rs"]
mod rtmp_support;
#[path = "../support/sse.rs"]
mod sse_support;
#[path = "../support/mod.rs"]
mod wire_support;

use std::{
    error::Error,
    fmt::Write as _,
    io,
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use bytes::{Bytes, BytesMut};
use http::{Method, Request, StatusCode};
use oxiroute_config::{
    AlpnProtocol, Certificate, CertificateSource, ConfigDraft, DownstreamTimeoutPolicy,
    ForwardAuditMode, ForwardConnectPolicy, ForwardDestinationPolicy, ForwardHeaderPolicy,
    ForwardHttpVersion, ForwardPeerPolicy, ForwardProxyService, ForwardResolverPolicy,
    HttpPathSelector, HttpRoute, HttpRouteAction, HttpRoutePolicy, HttpService, HttpVersionPolicy,
    L4Service, Listener, Management, Protocol, RtmpApplication, RtmpService, TlsProfile,
    TlsVersion, UpstreamAlgorithm, UpstreamConnectionReuse, UpstreamPool,
};
use rml_rtmp::handshake::{Handshake, PeerType};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore},
    task::{JoinHandle, JoinSet},
    time::{sleep, timeout},
};

use config_support::{empty_config, socket_bind, socket_endpoint};
use http_support::{HttpResponse, http_request};
use process_support::{ServerProcess, reserve_tcp_address, write_config};
use rtmp_support::RtmpWireClient;
use sse_support::{open_event_stream, read_chunk};

mod helpers;
mod scenarios;

#[allow(clippy::wildcard_imports)]
use helpers::*;

const TOKEN: &str = "7a89c4b6cefd8b4c11b6b4f9d1e6b5d0e8f1a2c3d4e5f60718293a4b5c6d7e8f";
const WIRE_TIMEOUT: Duration = Duration::from_secs(10);
const DRAIN_PROBE_TIMEOUT: Duration = Duration::from_millis(750);
static PROCESS_DRAIN_TEST_GATE: OnceLock<Arc<Semaphore>> = OnceLock::new();

type BoxError = Box<dyn Error + Send + Sync>;

async fn process_drain_test_guard() -> OwnedSemaphorePermit {
    PROCESS_DRAIN_TEST_GATE
        .get_or_init(|| Arc::new(Semaphore::new(1)))
        .clone()
        .acquire_owned()
        .await
        .expect("process drain test gate remains open")
}

struct H2Connection {
    sender: h2::client::SendRequest<Bytes>,
    driver: JoinHandle<Result<(), h2::Error>>,
}

impl H2Connection {
    async fn try_connect(address: SocketAddr) -> Result<Self, BoxError> {
        let stream = wire_support::tls_connect(
            address,
            wire_support::PROXY_SERVER_NAME,
            "ca-a.pem",
            &[b"h2"],
        )
        .await?;
        let (sender, connection) = h2::client::handshake(stream).await?;
        Ok(Self {
            sender,
            driver: tokio::spawn(connection),
        })
    }

    async fn get(&mut self) -> Result<Bytes, BoxError> {
        let mut sender = self.sender.clone().ready().await?;
        let request = Request::builder()
            .method(Method::GET)
            .uri(format!("https://{}/", wire_support::PROXY_SERVER_NAME))
            .body(())?;
        let (response, _request_body) = sender.send_request(request, true)?;
        let response = timeout(WIRE_TIMEOUT, response).await??;
        if response.status() != StatusCode::OK {
            return Err(
                io::Error::other(format!("unexpected H2 status {}", response.status())).into(),
            );
        }
        let mut body = response.into_body();
        let mut bytes = BytesMut::new();
        while let Some(chunk) = body.data().await {
            let chunk = chunk?;
            body.flow_control().release_capacity(chunk.len())?;
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes.freeze())
    }

    async fn connect(
        &mut self,
        destination: SocketAddr,
        payload: &[u8],
    ) -> Result<Bytes, BoxError> {
        let mut sender = self.sender.clone().ready().await?;
        let request = Request::builder()
            .method(Method::CONNECT)
            .uri(destination.to_string())
            .body(())?;
        let (response, mut request_body) = sender.send_request(request, false)?;
        request_body.send_data(Bytes::copy_from_slice(payload), true)?;
        let response = timeout(WIRE_TIMEOUT, response).await??;
        if response.status() != StatusCode::OK {
            return Err(io::Error::other(format!(
                "unexpected forward H2 status {}",
                response.status()
            ))
            .into());
        }
        let mut body = response.into_body();
        let chunk = timeout(WIRE_TIMEOUT, body.data())
            .await?
            .ok_or_else(|| io::Error::other("forward H2 tunnel ended early"))??;
        body.flow_control().release_capacity(chunk.len())?;
        Ok(chunk)
    }

    async fn finish(self) {
        drop(self.sender);
        self.driver.abort();
        let _ = self.driver.await;
    }
}
