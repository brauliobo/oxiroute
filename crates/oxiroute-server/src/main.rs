use std::{error::Error, fs, sync::Arc};

use async_trait::async_trait;
use log::{info, warn};
use oxiroute_config::load_lua;
use oxiroute_rtmp::{RtmpCapabilities, RtmpRegistry};
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
use tokio::io::copy_bidirectional;

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

fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();

    let config_path = std::env::args_os()
        .nth(1)
        .unwrap_or_else(|| "oxiroute.lua".into());
    let source = fs::read_to_string(&config_path)?;
    let config = load_lua(&source)?;

    let mut server = Server::new(None)?;
    server.bootstrap();

    let rtmp_registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: false,
        manual_recording: false,
    }));
    if let Some(management) = &config.management {
        let app = HttpServer::new_app(RtmpManagementApi::new(Arc::clone(&rtmp_registry)));
        let mut service = Service::new("OxiRoute management".into(), app);
        service.add_tcp(&management.bind.to_string());
        server.add_service(service);
        info!("configured management API on {}", management.bind);
    }

    for spec in service_specs(&config) {
        let bind = spec.bind.to_string();
        let service_name = format!("OxiRoute {}", spec.name);

        match spec.kind {
            ServiceKind::Http => {
                let proxy = HttpReverseProxy::new(spec.upstream);
                let mut service =
                    http_proxy_service_with_name(&server.configuration, proxy, &service_name);
                service.add_tcp(&bind);
                server.add_service(service);
            }
            ServiceKind::Tcp => {
                let mut service = Service::new(service_name, TcpRelay::new(spec.upstream));
                service.add_tcp(&bind);
                server.add_service(service);
            }
        }

        info!(
            "configured {} on {} -> {}",
            spec.name, spec.bind, spec.upstream
        );
    }

    server.run_forever()
}
