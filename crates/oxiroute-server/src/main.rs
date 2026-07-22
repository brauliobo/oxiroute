use std::{error::Error, fs, sync::Arc, time::SystemTime};

use async_trait::async_trait;
use log::{info, warn};
use oxiroute_config::load_lua;
use oxiroute_rtmp::{RtmpCapabilities, RtmpPublishSession, RtmpRegistry};
use oxiroute_server::{
    HttpReverseProxy, ListenerMetrics, MAX_HTTP_ATTEMPTS, MonitoredHttpApp, RtmpManagementApi,
    RuntimeMetrics, ServiceKind, TcpRelayCore, service_specs,
};
use pingora::{
    apps::ServerApp,
    apps::http_app::HttpServer,
    protocols::Stream,
    proxy::http_proxy,
    server::{Server, ShutdownWatch, configuration::ServerConf},
    services::listening::Service,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const RTMP_READ_BUFFER_SIZE: usize = 16 * 1024;

struct TcpRelay {
    service: Arc<oxiroute_server::L4ServicePlan>,
    metrics: ListenerMetrics,
}

impl TcpRelay {
    fn new(service: Arc<oxiroute_server::L4ServicePlan>, metrics: ListenerMetrics) -> Self {
        Self { service, metrics }
    }
}

#[async_trait]
impl ServerApp for TcpRelay {
    async fn process_new(
        self: &Arc<Self>,
        downstream: Stream,
        shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        let connection = match self.metrics.begin_connection() {
            Ok(connection) => connection,
            Err(error) => {
                warn!("could not account for TCP connection: {error}");
                return None;
            }
        };
        let relay = TcpRelayCore::new(self.service.select(), self.service.policy());
        if let Err(error) = relay.relay(downstream, &connection, shutdown.clone()).await {
            warn!("TCP relay failed: {error}");
        }

        None
    }
}

struct RtmpIngest {
    metrics: ListenerMetrics,
    server_id: String,
    registry: Arc<RtmpRegistry>,
}

impl RtmpIngest {
    fn new(server_id: String, registry: Arc<RtmpRegistry>, metrics: ListenerMetrics) -> Self {
        Self {
            metrics,
            server_id,
            registry,
        }
    }
}

#[async_trait]
impl ServerApp for RtmpIngest {
    async fn process_new(
        self: &Arc<Self>,
        mut downstream: Stream,
        shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        let connection = match self.metrics.begin_connection() {
            Ok(connection) => connection,
            Err(error) => {
                warn!("could not account for RTMP connection: {error}");
                return None;
            }
        };
        let Some(mut at_unix_ms) = unix_time_ms() else {
            warn!("cannot start RTMP session because the system clock is invalid");
            return None;
        };
        let mut session = RtmpPublishSession::new(&self.server_id, Arc::clone(&self.registry));
        let mut buffer = [0; RTMP_READ_BUFFER_SIZE];
        let mut shutdown = shutdown.clone();

        'connection: loop {
            let read = tokio::select! {
                result = downstream.read(&mut buffer) => Some(result),
                _ = shutdown.changed() => None,
            };
            let Some(read) = read else {
                break;
            };
            let bytes_read = match read {
                Ok(0) => break,
                Ok(bytes_read) => bytes_read,
                Err(error) => {
                    warn!("RTMP transport read failed: {error}");
                    break;
                }
            };
            let Ok(bytes_received) = u64::try_from(bytes_read) else {
                warn!("RTMP read size exceeds the supported metrics range");
                break;
            };
            if let Err(error) = connection.record_bytes_received(bytes_received) {
                warn!("could not account for RTMP ingress: {error}");
                break;
            }
            let Some(now_unix_ms) = unix_time_ms() else {
                warn!("closing RTMP session because the system clock is invalid");
                break;
            };
            at_unix_ms = now_unix_ms;

            let outbound = match session.receive(&buffer[..bytes_read], at_unix_ms) {
                Ok(outbound) => outbound,
                Err(error) => {
                    warn!("RTMP session failed: {error}");
                    break;
                }
            };
            let mut bytes_sent = 0_u64;
            for packet in &outbound {
                let Ok(packet_bytes) = u64::try_from(packet.len()) else {
                    warn!("RTMP response size exceeds the supported metrics range");
                    break 'connection;
                };
                let Some(total) = bytes_sent.checked_add(packet_bytes) else {
                    warn!("RTMP response byte total overflowed");
                    break 'connection;
                };
                bytes_sent = total;
                if let Err(error) = downstream.write_all(packet).await {
                    warn!("RTMP transport write failed: {error}");
                    break 'connection;
                }
            }
            if let Err(error) = downstream.flush().await {
                warn!("RTMP transport flush failed: {error}");
                break;
            }
            if let Err(error) = connection.record_bytes_sent(bytes_sent) {
                warn!("could not account for RTMP egress: {error}");
                break;
            }
        }

        if let Err(error) = session.close(at_unix_ms) {
            warn!("could not detach RTMP publisher: {error}");
        }
        None
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();

    let config_path = std::env::args_os()
        .nth(1)
        .unwrap_or_else(|| "oxiroute.lua".into());
    let source = fs::read_to_string(&config_path)?;
    let config = load_lua(&source)?;

    let server_config = ServerConf {
        max_retries: MAX_HTTP_ATTEMPTS,
        ..ServerConf::default()
    };
    let mut server = Server::new_with_opt_and_conf(None, server_config);
    server.bootstrap();

    let runtime_metrics = RuntimeMetrics::new();
    let services = service_specs(&config)?
        .into_iter()
        .map(|spec| {
            let metrics = runtime_metrics.register_listener(
                &spec.name,
                spec.kind.protocol(),
                spec.bind.to_string(),
                spec.max_connections,
            )?;
            Ok::<_, oxiroute_server::MetricsError>((spec, metrics))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let rtmp_registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: services
            .iter()
            .any(|(spec, _)| matches!(&spec.kind, ServiceKind::Rtmp)),
        manual_recording: false,
    }));
    if let Some(management) = &config.management {
        let management_api = if let Some(ui_dir) = &management.ui_dir {
            RtmpManagementApi::with_ui_dir(
                Arc::clone(&rtmp_registry),
                runtime_metrics.clone(),
                ui_dir,
            )?
        } else {
            RtmpManagementApi::new(Arc::clone(&rtmp_registry), runtime_metrics.clone())
        };
        let app = HttpServer::new_app(management_api);
        let mut service = Service::new("OxiRoute management".into(), app);
        service.add_tcp(&management.bind.to_string());
        server.add_service(service);
        info!("configured management API on {}", management.bind);
    }

    for (spec, metrics) in services {
        let bind = spec.bind.to_string();
        let service_name = format!("OxiRoute {}", spec.name);

        match spec.kind {
            ServiceKind::Http(http_service) => {
                let proxy = http_proxy(
                    &server.configuration,
                    HttpReverseProxy::new(http_service, metrics.clone()),
                );
                let mut service = Service::new(service_name, MonitoredHttpApp::new(proxy, metrics));
                service.add_tcp(&bind);
                server.add_service(service);
            }
            ServiceKind::Rtmp => {
                let mut service = Service::new(
                    service_name,
                    RtmpIngest::new(spec.name.clone(), Arc::clone(&rtmp_registry), metrics),
                );
                service.add_tcp(&bind);
                server.add_service(service);
            }
            ServiceKind::Tcp(l4_service) => {
                let mut service = Service::new(service_name, TcpRelay::new(l4_service, metrics));
                service.add_tcp(&bind);
                server.add_service(service);
            }
        }

        info!("configured {} on {}", spec.name, spec.bind);
    }

    server.run_forever()
}

fn unix_time_ms() -> Option<u64> {
    let duration = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?;
    u64::try_from(duration.as_millis()).ok()
}
