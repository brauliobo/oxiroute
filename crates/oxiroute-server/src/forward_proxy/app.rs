#[allow(clippy::wildcard_imports)]
use super::*;

/// H2 uses the same compiled plan as H1; only the downstream stream adapter differs.
pub type ForwardHttp2ServicePlan = ForwardHttp1ServicePlan;

pub struct ForwardHttp2ServiceApp {
    service: Arc<ForwardHttp2ServicePlan>,
}

impl ForwardHttp2ServiceApp {
    #[must_use]
    pub fn new(service: Arc<ForwardHttp2ServicePlan>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl HttpServerApp for ForwardHttp2ServiceApp {
    async fn process_new_http(
        self: &Arc<Self>,
        session: ServerSession,
        shutdown: &ShutdownWatch,
    ) -> Option<ReusedHttpStream> {
        self.service.process_new_http(session, shutdown).await
    }

    fn h2_options(&self) -> Option<H2Options> {
        self.service.h2_options()
    }

    fn server_options(&self) -> Option<&HttpServerOptions> {
        self.service.server_options()
    }
}

#[async_trait]
impl HttpServerApp for ForwardHttp1ServicePlan {
    #[allow(clippy::too_many_lines)]
    async fn process_new_http(
        self: &Arc<Self>,
        mut session: ServerSession,
        shutdown: &ShutdownWatch,
    ) -> Option<ReusedHttpStream> {
        if !session.is_http2() {
            // This plan is selected by a ForwardHttp2 listener. Never reinterpret H1 as H2.
            session.shutdown().await;
            return None;
        }
        if session.req_header().method != Method::CONNECT {
            self.reject_h2(&mut session, RequestFailure::BadRequest)
                .await;
            return None;
        }
        let Ok(request) = h2_request(&session) else {
            self.reject_h2(&mut session, RequestFailure::BadRequest)
                .await;
            return None;
        };
        let authorized = match self
            .authorize_request(
                &request,
                Protocol::Http2,
                session
                    .client_addr()
                    .and_then(|address| address.as_inet().copied()),
            )
            .await
        {
            Ok(request) => request,
            Err(error) => {
                self.reject_h2(&mut session, error).await;
                return None;
            }
        };
        let AuthorizedRequest {
            approved,
            parsed: ParsedTarget::Tunnel(destination),
            lifetime_deadline,
            ..
        } = authorized
        else {
            self.reject_h2(&mut session, RequestFailure::BadRequest)
                .await;
            return None;
        };
        let connect_deadline = lifetime_deadline.min(Instant::now() + self.connect_timeout);
        let upstream = match timeout_at(
            lifetime_deadline,
            self.connect_tcp_until(approved.socket_addresses(), connect_deadline),
        )
        .await
        {
            Ok(Ok(upstream)) => upstream,
            Ok(Err(error)) => {
                self.reject_h2(&mut session, error).await;
                return None;
            }
            Err(_) => {
                self.reject_h2(&mut session, RequestFailure::GatewayTimeout)
                    .await;
                return None;
            }
        };
        let Ok(response_header) = pingora::http::ResponseHeader::build(StatusCode::OK, None) else {
            self.reject_h2(&mut session, RequestFailure::BadGateway)
                .await;
            return None;
        };
        if session
            .write_response_header(Box::new(response_header))
            .await
            .is_err()
        {
            return None;
        }
        let lifetime_timeout = lifetime_deadline.saturating_duration_since(Instant::now());
        if lifetime_timeout.is_zero() {
            session.shutdown().await;
            return None;
        }
        let Ok(tunnel) = BoundedTunnel::new(TunnelLimits {
            idle_timeout: self.idle_timeout,
            lifetime_timeout,
            ..TunnelLimits::default()
        }) else {
            session.shutdown().await;
            return None;
        };
        let mut shutdown = shutdown.clone();
        let outcome = tokio::select! {
            outcome = tunnel.relay_h2(
                PingoraH2Stream { session },
                upstream,
            ) => Some(outcome),
            _ = shutdown.changed() => None,
        };
        match outcome.as_ref() {
            Some(outcome) => log::info!(
                target: "oxiroute::forward_proxy",
                "event=forward_tunnel protocol=h2 destination={} outcome={} bytes_left_to_right={} bytes_right_to_left={}",
                destination.authority(),
                outcome.kind().as_str(),
                outcome.stats().left_to_right,
                outcome.stats().right_to_left,
            ),
            None => log::info!(
                target: "oxiroute::forward_proxy",
                "event=forward_tunnel protocol=h2 destination={} outcome=cancelled bytes_left_to_right=0 bytes_right_to_left=0",
                destination.authority(),
            ),
        }
        None
    }

    fn h2_options(&self) -> Option<H2Options> {
        let mut options = default_h2_options();
        options.max_header_list_size(u32::try_from(self.max_header_bytes).unwrap_or(u32::MAX));
        Some(options)
    }

    fn server_options(&self) -> Option<&HttpServerOptions> {
        Some(&self.http_server_options)
    }
}

struct PingoraH2Stream {
    session: ServerSession,
}

#[async_trait]
impl H2TunnelStream for PingoraH2Stream {
    async fn recv_data(&mut self) -> io::Result<Option<Bytes>> {
        let data = self
            .session
            .read_request_body()
            .await
            .map_err(|error| pingora_io_error(&error))?;
        if data
            .as_ref()
            .is_some_and(|data| data.is_empty() && self.session.is_body_done())
        {
            Ok(None)
        } else {
            Ok(data)
        }
    }

    async fn send_data(&mut self, data: Bytes, end: bool) -> io::Result<()> {
        self.session
            .write_response_body(data, end)
            .await
            .map_err(|error| pingora_io_error(&error))
    }

    async fn wait_closed(&mut self) -> io::Result<()> {
        self.session
            .read_body_or_idle(true)
            .await
            .map(|_| ())
            .map_err(|error| pingora_io_error(&error))
    }

    async fn reset(&mut self) {
        self.session.shutdown().await;
    }
}

fn pingora_io_error(error: &pingora::Error) -> io::Error {
    io::Error::other(error.to_string())
}

fn h2_request(session: &ServerSession) -> Result<Request<()>, ()> {
    let header = session.req_header();
    let mut request = Request::builder()
        .method(header.method.clone())
        .uri(header.uri.clone())
        .body(())
        .map_err(|_| ())?;
    *request.headers_mut() = header.headers.clone();
    Ok(request)
}
