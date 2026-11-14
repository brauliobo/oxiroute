#[path = "../support/config.rs"]
mod config_support;
#[path = "../support/fixtures.rs"]
mod fixture_support;
#[path = "../support/http.rs"]
mod http_support;
#[path = "../support/ports.rs"]
mod ports_support;
#[path = "../support/rtmp.rs"]
mod rtmp_support;

use std::{
    fmt::Write as _,
    fs,
    net::{Ipv4Addr, SocketAddr},
    os::{
        fd::IntoRawFd,
        unix::fs::{PermissionsExt as _, symlink},
    },
    path::PathBuf,
    sync::{Arc, OnceLock},
    thread,
    time::{Duration, Instant},
};

use config_support::render_lua;
use oxiroute_config::{
    Certificate, CertificateSource, ConfigDraft, Listener, Management, Protocol, RtmpAccessPolicy,
    RtmpApplication, RtmpDashPolicy, RtmpDashSegmentNaming, RtmpRecorderStart, RtmpService,
    RtmpSessionCeilings, RtmpTokenPolicy, RtmpTokenSource, RtmpVodPolicy, RtmpVodSource,
    ValidatedConfig,
};
use oxiroute_rtmp::{
    PreparedRtmpRuntimeSet, RtmpControlHandle, RtmpPrepareContext, RtmpPrepareMode, RtmpRuntimeSet,
    RtmpServiceHandle, RtmpSession, RtmpSessionControlAction,
};
use oxiroute_server::{
    GenerationManager, HttpListenerApp, RtmpManagementApi, RuntimeMetrics, RuntimeMode,
    ServiceKind, TopologySnapshot,
    config_coordinator::{
        AuthoredRevision, CanonicalConfigCoordinator, ConfigLoadOutcome, EffectiveRevision,
        MAX_CANONICAL_CONFIG_BYTES,
    },
    emit_certificate,
};
use pingora::{
    server::Fds,
    services::{Service as PingoraService, listening::Service as ListeningService},
};
use serde_json::Value;
use tempfile::TempDir;

use rml_rtmp::{
    chunk_io::ChunkSerializer, messages::RtmpMessage, rml_amf0::Amf0Value, time::RtmpTimestamp,
};
use rtmp_support::RtmpSessionClient;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
    sync::watch,
    task::JoinHandle,
};

use config_support::{empty_config, loopback_address, rtmp_recorder, runtime_plan, socket_bind};
use fixture_support::{fixture, write_file_with_mode};
use http_support::{HttpResponse, http_request, raw_http_request};

mod config_apply;
mod config_requests;
mod events;
mod helpers;
mod status;

#[allow(clippy::wildcard_imports)]
use helpers::*;

const TEST_TOKEN: &str = "cdb85a91948758cfcb895216a3603c8fcd8aaf691f39f5fd82b5df15af14628e";

fn reserve_tcp_address() -> SocketAddr {
    ports_support::unused_address()
}

struct ManagementHarness {
    active_revision: EffectiveRevision,
    disk_revision: AuthoredRevision,
    address: SocketAddr,
    config_path: PathBuf,
    shutdown: watch::Sender<bool>,
    task: JoinHandle<()>,
    rtmp_runtime: RtmpRuntimeSet,
    _directory: TempDir,
}

impl ManagementHarness {
    async fn start(config: &ValidatedConfig) -> Self {
        let source = render_lua(config).expect("active config renders");
        Self::start_source(config, "lua", source.as_bytes()).await
    }

    async fn start_source(config: &ValidatedConfig, extension: &str, source: &[u8]) -> Self {
        Self::start_source_with_generation_manager(
            config,
            extension,
            source,
            false,
            RuntimeMode::Direct,
        )
        .await
    }

    async fn start_event_api(config: &ValidatedConfig) -> Self {
        let source = render_lua(config).expect("active config renders");
        Self::start_source_with_generation_manager(
            config,
            "lua",
            source.as_bytes(),
            true,
            RuntimeMode::Direct,
        )
        .await
    }

    async fn start_source_with_generation_manager(
        config: &ValidatedConfig,
        extension: &str,
        source: &[u8],
        enable_generation_manager: bool,
        mode: RuntimeMode,
    ) -> Self {
        let directory = TempDir::new().expect("temporary canonical config directory");
        let config_path = directory.path().join(format!("oxiroute.{extension}"));
        fs::write(&config_path, source).expect("write active config");
        let coordinator = CanonicalConfigCoordinator::new(&config_path).expect("coordinator");
        let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
            panic!("active config must load")
        };
        let active_revision = document.effective_revision.clone();
        let disk_revision = document.authored_revision.clone();
        let plan = oxiroute_server::runtime_plan(config).expect("active runtime plan");
        let services = oxiroute_server::service_specs(config).expect("active runtime services");
        let rtmp_services = services
            .iter()
            .filter_map(|service| match &service.kind {
                ServiceKind::Rtmp(service) => Some(service),
                _ => None,
            })
            .collect::<Vec<_>>();
        let context = RtmpPrepareContext::new(
            RtmpPrepareMode::Activation,
            services.iter().filter_map(|service| match service.bind {
                oxiroute_config::ListenerBind::Socket { address, .. }
                | oxiroute_config::ListenerBind::Udp { address } => Some(address),
                oxiroute_config::ListenerBind::Unix { .. } => None,
            }),
        );
        let mut plans = Vec::new();
        for service in rtmp_services {
            if !plans.iter().any(|plan: &oxiroute_rtmp::RtmpServicePlan| {
                plan.service_id() == service.service_id()
            }) {
                plans.push(service.value_plan());
            }
        }
        let rtmp_runtime = PreparedRtmpRuntimeSet::prepare(
            plans,
            &context,
            Instant::now() + Duration::from_secs(1),
        )
        .expect("prepared RTMP test runtime")
        .start(Instant::now() + Duration::from_secs(1))
        .expect("RTMP test runtime");
        let control = rtmp_runtime.control();
        let metrics = RuntimeMetrics::new();
        metrics.set_rtmp_recording_supported(plan.rtmp_recording_supported);
        let mut api = RtmpManagementApi::new(control, metrics, Arc::clone(&plan.topology))
            .with_config_coordinator(coordinator, active_revision.clone(), TEST_TOKEN, mode)
            .expect("injected management token");
        if enable_generation_manager {
            let manager = match mode {
                RuntimeMode::Direct => GenerationManager::new(),
                RuntimeMode::Supervised => GenerationManager::new_supervised(),
            };
            let candidate = manager
                .prepare((*document).clone())
                .expect("active generation candidate");
            let mut startup = manager
                .begin_candidate_start(&candidate)
                .expect("active generation startup");
            let generation = startup
                .claim_runtime_start()
                .expect("active generation runtime claim");
            assert!(generation.mark_runtime_started());
            startup.activate().expect("active generation");
            api = api.with_generation_manager(manager);
        }

        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("reserve management listener");
        listener
            .set_nonblocking(true)
            .expect("make management listener nonblocking");
        let address = listener.local_addr().expect("management listener address");
        let http_server = HttpListenerApp::new(api.into_http_app(), None);
        let mut service = ListeningService::new("OxiRoute management API test".into(), http_server);
        service.add_tcp(&address.to_string());
        let mut inherited = Fds::new();
        inherited.add(address.to_string(), listener.into_raw_fd());
        let inherited = Arc::new(parking_lot::Mutex::new(inherited));
        let (shutdown, shutdown_watch) = watch::channel(false);
        let task = tokio::spawn(async move {
            PingoraService::start_service(&mut service, Some(inherited), shutdown_watch, 1).await;
        });
        tokio::task::yield_now().await;

        Self {
            active_revision,
            disk_revision,
            address,
            config_path,
            shutdown,
            task,
            rtmp_runtime,
            _directory: directory,
        }
    }

    fn rtmp_runtime(&self) -> RtmpServiceHandle {
        self.rtmp_runtime
            .service("live")
            .expect("RTMP service runtime in test configuration")
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        config_revision: Option<&str>,
        body: Option<&Value>,
    ) -> HttpResponse {
        self.request_with(
            method,
            path,
            Some(TEST_TOKEN),
            config_revision,
            body.map(|_| "application/json"),
            body,
        )
        .await
    }

    async fn request_with(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        config_revision: Option<&str>,
        content_type: Option<&str>,
        body: Option<&Value>,
    ) -> HttpResponse {
        let body = body.map(Value::to_string).unwrap_or_default();
        let mut headers = Vec::new();
        if let Some(token) = token {
            headers.push(("Authorization", format!("Bearer {token}")));
        }
        if let Some(config_revision) = config_revision {
            headers.push(("If-Config-Revision", config_revision.to_owned()));
        }
        if let Some(content_type) = content_type {
            headers.push(("Content-Type", content_type.to_owned()));
        }
        let headers = headers
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect::<Vec<_>>();
        http_request(self.address, method, path, &headers, body.as_bytes()).await
    }

    async fn raw_request(&self, request: Vec<u8>) -> HttpResponse {
        raw_http_request(self.address, &request).await
    }

    async fn open_event_stream(&self, last_event_id: Option<u64>) -> (TcpStream, Vec<u8>) {
        let mut stream = TcpStream::connect(self.address)
            .await
            .expect("connect event stream");
        let mut request = format!(
            "GET /api/v1/events/stream HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {TEST_TOKEN}\r\nAccept: text/event-stream\r\nConnection: close\r\n"
        );
        if let Some(last_event_id) = last_event_id {
            let _ = write!(request, "Last-Event-ID: {last_event_id}\r\n");
        }
        request.push_str("\r\n");
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write event stream request");
        let head = read_response_head(&mut stream).await;
        (stream, head)
    }

    async fn open_versioned_event_stream(&self, path: &str) -> (TcpStream, Vec<u8>) {
        let mut stream = TcpStream::connect(self.address)
            .await
            .expect("connect versioned event stream");
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {TEST_TOKEN}\r\nAccept: text/event-stream\r\nConnection: close\r\n\r\n"
        );
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write versioned event stream request");
        let head = read_response_head(&mut stream).await;
        assert!(String::from_utf8_lossy(&head).starts_with("HTTP/1.1 200"));
        (stream, head)
    }
}

impl Drop for ManagementHarness {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        self.task.abort();
    }
}
