use std::{error::Error, fs, sync::Arc};

use async_trait::async_trait;
use log::{info, warn};
use oxiroute_config::load_lua;
use oxiroute_server::{service_specs, ServiceKind};
use pingora::{
    apps::ServerApp,
    connectors::TransportConnector,
    protocols::Stream,
    proxy::{http_proxy_service_with_name, ProxyHttp, Session},
    server::{Server, ShutdownWatch},
    services::listening::Service,
    upstreams::peer::{BasicPeer, HttpPeer},
};
use tokio::io::copy_bidirectional;

struct HttpReverseProxy {
    upstream: std::net::SocketAddr,
}

#[async_trait]
impl ProxyHttp for HttpReverseProxy {
    type CTX = ();

    fn new_ctx(&self) -> Self::CTX {}

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        _ctx: &mut Self::CTX,
    ) -> pingora::Result<Box<HttpPeer>> {
        Ok(Box::new(HttpPeer::new(self.upstream, false, String::new())))
    }
}

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

    for spec in service_specs(&config) {
        let bind = spec.bind.to_string();
        let service_name = format!("OxiRoute {}", spec.name);

        match spec.kind {
            ServiceKind::Http => {
                let proxy = HttpReverseProxy {
                    upstream: spec.upstream,
                };
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
