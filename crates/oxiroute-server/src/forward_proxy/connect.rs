#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) struct ConnectedHttp {
    pub(super) stream: BoxedIo,
    pub(super) via_peer: bool,
}

impl ForwardHttp1ServicePlan {
    pub(super) async fn connect_tcp_until(
        &self,
        addresses: &[SocketAddr],
        deadline: Instant,
    ) -> Result<TcpStream, RequestFailure> {
        for address in addresses {
            match timeout_at(deadline, TcpStream::connect(address)).await {
                Ok(Ok(stream)) => return Ok(stream),
                Ok(Err(_)) => {}
                Err(_) => return Err(RequestFailure::GatewayTimeout),
            }
        }
        Err(RequestFailure::BadGateway)
    }

    pub(super) async fn connect_udp_until(
        &self,
        addresses: &[SocketAddr],
        deadline: Instant,
    ) -> Result<UdpSocket, RequestFailure> {
        for address in addresses {
            let local = SocketAddr::new(
                if address.is_ipv4() {
                    IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
                } else {
                    IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED)
                },
                0,
            );
            let socket = match timeout_at(deadline, UdpSocket::bind(local)).await {
                Ok(Ok(socket)) => socket,
                Ok(Err(_)) => continue,
                Err(_) => return Err(RequestFailure::GatewayTimeout),
            };
            match timeout_at(deadline, socket.connect(address)).await {
                Ok(Ok(())) => return Ok(socket),
                Ok(Err(_)) => {}
                Err(_) => return Err(RequestFailure::GatewayTimeout),
            }
        }
        Err(RequestFailure::BadGateway)
    }

    pub(super) async fn connect_tunnel_until(
        &self,
        destination: &Destination,
        direct_addresses: &[SocketAddr],
        deadline: Instant,
    ) -> Result<BoxedIo, RequestFailure> {
        let mut last_error = RequestFailure::BadGateway;
        if self.peer_direct_fallback != ForwardDirectFallback::Required {
            let attempts = self
                .peers
                .len()
                .min(self.peer_max_retries.saturating_add(1));
            for peer in self.peers.iter().take(attempts) {
                match self.connect_peer_tunnel(peer, destination, deadline).await {
                    Ok(stream) => return Ok(stream),
                    Err(error) => last_error = error,
                }
                if Instant::now() >= deadline {
                    break;
                }
            }
            if self.peer_direct_fallback == ForwardDirectFallback::Denied {
                return Err(last_error);
            }
        }
        self.connect_tcp_until(direct_addresses, deadline)
            .await
            .map(|stream| Box::new(stream) as BoxedIo)
    }

    async fn connect_peer_tunnel(
        &self,
        peer: &StaticPeerPlan,
        destination: &Destination,
        deadline: Instant,
    ) -> Result<BoxedIo, RequestFailure> {
        let stream = self.connect_peer_tcp(peer, deadline).await?;
        self.connect_through_peer(stream, destination, deadline)
            .await
    }

    async fn connect_peer_tcp(
        &self,
        peer: &StaticPeerPlan,
        deadline: Instant,
    ) -> Result<TcpStream, RequestFailure> {
        let peer_destination = Destination {
            host: peer.host.clone(),
            port: peer.port,
        };
        let addresses = self.resolve(&peer_destination).await?;
        let socket_addresses = addresses
            .into_iter()
            .map(|address| SocketAddr::new(address, peer.port))
            .collect::<Vec<_>>();
        self.connect_tcp_until(&socket_addresses, deadline).await
    }

    async fn connect_through_peer(
        &self,
        stream: TcpStream,
        destination: &Destination,
        deadline: Instant,
    ) -> Result<BoxedIo, RequestFailure> {
        let (mut sender, connection) = timeout_at(
            deadline,
            hyper::client::conn::http1::handshake(TokioIo::new(stream)),
        )
        .await
        .map_err(|_| RequestFailure::GatewayTimeout)?
        .map_err(|_| RequestFailure::BadGateway)?;
        tokio::spawn(async move {
            let _ = connection.with_upgrades().await;
        });
        let request = Request::builder()
            .method(Method::CONNECT)
            .uri(destination.authority())
            .header(header::HOST, destination.authority())
            .body(Full::new(Bytes::new()))
            .map_err(|_| RequestFailure::BadRequest)?;
        let response = timeout_at(deadline, sender.send_request(request))
            .await
            .map_err(|_| RequestFailure::GatewayTimeout)?
            .map_err(|_| RequestFailure::BadGateway)?;
        if response.status() != StatusCode::OK {
            return Err(RequestFailure::BadGateway);
        }
        let upgraded = timeout_at(deadline, hyper::upgrade::on(response))
            .await
            .map_err(|_| RequestFailure::GatewayTimeout)?
            .map_err(|_| RequestFailure::BadGateway)?;
        Ok(Box::new(TokioIo::new(upgraded)))
    }

    pub(super) async fn connect_http_with_peers(
        &self,
        destination: &Destination,
        scheme: ForwardScheme,
        direct_addresses: &[SocketAddr],
        deadline: Instant,
    ) -> Result<ConnectedHttp, RequestFailure> {
        let mut last_error = RequestFailure::BadGateway;
        if self.peer_direct_fallback != ForwardDirectFallback::Required {
            let attempts = self
                .peers
                .len()
                .min(self.peer_max_retries.saturating_add(1));
            for peer in self.peers.iter().take(attempts) {
                match self
                    .connect_http_through_peer(peer, destination, scheme, deadline)
                    .await
                {
                    Ok(stream) => {
                        return Ok(ConnectedHttp {
                            stream,
                            via_peer: true,
                        });
                    }
                    Err(error) => last_error = error,
                }
                if Instant::now() >= deadline {
                    break;
                }
            }
            if self.peer_direct_fallback == ForwardDirectFallback::Denied {
                return Err(last_error);
            }
        }
        self.connect_http(destination, scheme, direct_addresses, deadline)
            .await
            .map(|stream| ConnectedHttp {
                stream,
                via_peer: false,
            })
    }

    async fn connect_http_through_peer(
        &self,
        peer: &StaticPeerPlan,
        destination: &Destination,
        scheme: ForwardScheme,
        deadline: Instant,
    ) -> Result<BoxedIo, RequestFailure> {
        let stream = self.connect_peer_tcp(peer, deadline).await?;
        if scheme == ForwardScheme::Http {
            return Ok(Box::new(stream));
        }
        let tunnel = self
            .connect_through_peer(stream, destination, deadline)
            .await?;
        self.connect_tls_stream(destination, tunnel, deadline).await
    }

    async fn connect_http(
        &self,
        destination: &Destination,
        scheme: ForwardScheme,
        addresses: &[SocketAddr],
        deadline: Instant,
    ) -> Result<BoxedIo, RequestFailure> {
        let stream = self.connect_tcp_until(addresses, deadline).await?;
        if scheme == ForwardScheme::Http {
            return Ok(Box::new(stream));
        }
        self.connect_tls_stream(destination, Box::new(stream), deadline)
            .await
    }

    async fn connect_tls_stream(
        &self,
        destination: &Destination,
        stream: BoxedIo,
        deadline: Instant,
    ) -> Result<BoxedIo, RequestFailure> {
        let tls_identity = match &destination.host {
            Host::Dns(host) => host.clone(),
            Host::Ip(address) => address.to_string(),
        };
        let ssl = self
            .tls_connector
            .configure()
            .and_then(|configuration| configuration.into_ssl(&tls_identity))
            .map_err(|_| RequestFailure::BadGateway)?;
        let mut stream = SslStream::new(ssl, stream).map_err(|_| RequestFailure::BadGateway)?;
        timeout_at(deadline, Pin::new(&mut stream).connect())
            .await
            .map_err(|_| RequestFailure::GatewayTimeout)?
            .map_err(|_| RequestFailure::BadGateway)?;
        Ok(Box::new(stream))
    }
}

pub(super) fn acquire_resolver(
    blueprint: &ForwardResolverBlueprint,
) -> Result<TokioResolver, ForwardPlanError> {
    let builder = if blueprint.nameservers.is_empty() {
        let (config, _) = hickory_resolver::system_conf::read_system_conf()
            .map_err(|_| ForwardPlanError::Resolver)?;
        TokioResolver::builder_with_config(config, TokioRuntimeProvider::default())
    } else {
        let nameservers = blueprint
            .nameservers
            .iter()
            .copied()
            .map(NameServerConfig::udp_and_tcp)
            .collect();
        TokioResolver::builder_with_config(
            ResolverConfig::from_parts(None, Vec::new(), nameservers),
            TokioRuntimeProvider::default(),
        )
    };
    builder
        .with_options(blueprint.options.clone())
        .build()
        .map_err(|_| ForwardPlanError::Resolver)
}
