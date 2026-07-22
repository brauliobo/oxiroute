use std::{error::Error, fs, sync::Arc, time::SystemTime};

use async_trait::async_trait;
use log::{info, warn};
use oxiroute_config::load_lua;
use oxiroute_rtmp::{RtmpCapabilities, RtmpPublishSession, RtmpRegistry};
use oxiroute_server::{service_specs, HttpReverseProxy, RtmpManagementApi, ServiceKind};
use pingora::{
    apps::http_app::HttpServer,
    apps::ServerApp,
    connectors::TransportConnector,
    protocols::Stream,
    proxy::http_proxy_service_with_name,
    server::{Server, ShutdownWatch},
    services::listening::Service,
    upstreams::peer::BasicPeer,
};
use tokio::io::{copy_bidirectional, AsyncReadExt, AsyncWriteExt};

const RTMP_READ_BUFFER_SIZE: usize = 16 * 1024;

struct TcpRelay {
    connector: TransportConnector,
    peer: BasicPeer,
}

impl TcpRelay {
    fn new(upstream: std::net::SocketAddr) -> Self {
        Self {
            connector: TransportConnector::new(None),
            peer: BasicPeer::new(&upstream.to_string()),
        }
    }
}

#[async_trait]
impl ServerApp for TcpRelay {
    async fn process_new(
        self: &Arc<Self>,
        mut downstream: Stream,
        _shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        match self.connector.new_stream(&self.peer).await {
            Ok(mut upstream) => {
                if let Err(error) = copy_bidirectional(&mut downstream, &mut upstream).await {
                    warn!("TCP relay to {} failed: {error}", self.peer);
                }
            }
            Err(error) => warn!("could not connect TCP upstream {}: {error}", self.peer),
        }

        None
    }
}

struct RtmpIngest {
    server_id: String,
    registry: Arc<RtmpRegistry>,
}

impl RtmpIngest {
    fn new(server_id: String, registry: Arc<RtmpRegistry>) -> Self {
        Self {
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
            for packet in outbound {
                if let Err(error) = downstream.write_all(&packet).await {
                    warn!("RTMP transport write failed: {error}");
                    break 'connection;
                }
            }
            if let Err(error) = downstream.flush().await {
                warn!("RTMP transport flush failed: {error}");
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

    let mut server = Server::new(None)?;
    server.bootstrap();

    let services = service_specs(&config)?;
    let rtmp_registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: services.iter().any(|spec| spec.kind == ServiceKind::Rtmp),
        manual_recording: false,
    }));
    if let Some(management) = &config.management {
        let management_api = if let Some(ui_dir) = &management.ui_dir {
            RtmpManagementApi::with_ui_dir(Arc::clone(&rtmp_registry), ui_dir)?
        } else {
            RtmpManagementApi::new(Arc::clone(&rtmp_registry))
        };
        let app = HttpServer::new_app(management_api);
        let mut service = Service::new("OxiRoute management".into(), app);
        service.add_tcp(&management.bind.to_string());
        server.add_service(service);
        info!("configured management API on {}", management.bind);
    }

    for spec in services {
        let bind = spec.bind.to_string();
        let service_name = format!("OxiRoute {}", spec.name);

        match spec.kind {
            ServiceKind::Http(upstream) => {
                let proxy = HttpReverseProxy::new(upstream);
                let mut service =
                    http_proxy_service_with_name(&server.configuration, proxy, &service_name);
                service.add_tcp(&bind);
                server.add_service(service);
            }
            ServiceKind::Rtmp => {
                let mut service = Service::new(
                    service_name,
                    RtmpIngest::new(spec.name.clone(), Arc::clone(&rtmp_registry)),
                );
                service.add_tcp(&bind);
                server.add_service(service);
            }
            ServiceKind::Tcp(upstream) => {
                let mut service = Service::new(service_name, TcpRelay::new(upstream));
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
