//! The compiled fast lane: HTTP/1 requests of the common shape are served on the connection itself, before any
//! Pingora session exists. A request the lane cannot prove equivalent to the classic path (a head it does not
//! fully understand, a route that needs a hook, a body, an upgrade, pipelined bytes) is handed to the classic path
//! with the bytes already read, so the classic path stays the semantic reference and nothing is read twice.
//!
//! The lane exists for requests whose cost is the layers around them; it never grows the grammar, it refuses a
//! superset of what the classic parser refuses.

mod buffers;
mod direct;
mod head;
mod plan;
mod proxy;
mod timing;

use std::{
    future::poll_fn,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use bytes::BytesMut;
use http::{HeaderValue, Method, uri::Authority};
use oxiroute_acme::ChallengeStore;
use oxiroute_config::is_unambiguous_http_path;
use oxiroute_h1_head::RequestHead;
use pingora::{
    apps::{Http1Connection, Http1Front, Http1FrontOutcome},
    connectors::http::v1::Connector,
    protocols::Stream,
    server::ShutdownWatch,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    sync::Notify,
};

pub(crate) use plan::LaneRoute;

use crate::{
    HttpOperationResult, HttpServicePlan, ListenerMetrics, RuntimeGeneration,
    http::{
        action::{HttpActionPlan, HttpRoutePlan},
        request_id::generated_request_id_bytes,
    },
};

/// The downstream timeouts of the listener, as the classic path applies them to a session.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LaneTimeouts {
    pub(crate) request: Option<Duration>,
    pub(crate) write: Option<Duration>,
    pub(crate) keepalive: Option<Duration>,
}

impl LaneTimeouts {
    /// The write timeout a session has: the request timeout caps it.
    fn session_write(&self) -> Option<Duration> {
        match (self.request, self.write) {
            (Some(request), Some(write)) => Some(request.min(write)),
            (request, None) => request,
            (None, write) => write,
        }
    }
}

pub(crate) struct LaneFront {
    challenge: Option<ChallengeStore>,
    first_request: Notify,
    generation: Arc<RuntimeGeneration>,
    metrics: ListenerMetrics,
    service: Arc<HttpServicePlan>,
    timeouts: LaneTimeouts,
    /// The upstream connector of the classic proxy, whose idle connections the proxied lane shares.
    upstream: Arc<Connector>,
    /// Time a response write may take before the connection is given up: the least of the service deadline and
    /// the downstream timeouts.
    write_budget: Duration,
}

enum Served {
    Next,
    Close,
    Handoff,
}

impl LaneFront {
    pub(crate) fn new(
        service: Arc<HttpServicePlan>,
        metrics: ListenerMetrics,
        generation: Arc<RuntimeGeneration>,
        challenge: Option<ChallengeStore>,
        timeouts: LaneTimeouts,
        upstream: Arc<Connector>,
        enabled: bool,
    ) -> Option<Self> {
        let eligible = enabled
            && service.gzip().is_none()
            && service.access_log().is_none()
            && service.automatic_response_headers();
        let write_budget = [
            Some(service.upstream_io_timeout()),
            timeouts.request,
            timeouts.write,
        ]
        .into_iter()
        .flatten()
        .min()?;
        eligible.then_some(Self {
            challenge,
            first_request: Notify::new(),
            generation,
            metrics,
            service,
            timeouts,
            upstream,
            write_budget,
        })
    }

    /// How long a wait for the first byte of a request may last: what `read_request` of a session would allow.
    fn idle_timeout(&self, connection: &Http1Connection, stopping: bool) -> Option<Duration> {
        if connection.is_reused() {
            return self.timeouts.keepalive;
        }
        self.timeouts.request.or_else(|| {
            (self.timeouts.keepalive.is_none() && !stopping).then_some(Duration::from_mins(1))
        })
    }

    /// Waits until the connection has bytes of a request and reads them. `None` when the connection is over: the
    /// client left, the wait timed out or the service is stopping. No buffer is held while the connection waits.
    async fn wait_for_bytes(
        &self,
        stream: &mut Stream,
        connection: &Http1Connection,
        shutdown: &ShutdownWatch,
    ) -> Option<BytesMut> {
        let stopping = *shutdown.borrow();
        let read = async {
            poll_fn(|cx| stream.poll_read_ready(cx)).await.ok()?;
            let mut buffer = buffers::take_read();
            match stream.read_buf(&mut buffer).await {
                Ok(count) if count > 0 => Some(buffer),
                _ => {
                    buffers::give_read(buffer);
                    None
                }
            }
        };
        let timed = async {
            match self.idle_timeout(connection, stopping) {
                Some(limit) => tokio::time::timeout(limit, read).await.unwrap_or(None),
                None => read.await,
            }
        };
        // A connection waiting between requests ends with the signal its owner shares; the first request of a
        // connection ends when the service stops.
        tokio::select! {
            biased;
            buffer = timed => buffer,
            () = async {
                if connection.is_reused() {
                    let _ = shutdown.clone().wait_for(|stopping| *stopping).await;
                } else {
                    self.first_request.notified().await;
                }
            } => None,
        }
    }

    /// The route the request is served by, when the lane may serve it, and the authority it was selected for.
    fn select_route(
        &self,
        buffer: &[u8],
        request: &head::Request,
    ) -> Option<(Arc<HttpRoutePlan>, Authority)> {
        let path = request.path(buffer);
        let authority = request.host(buffer)?.parse::<Authority>().ok()?;
        if authority.as_str().contains('@') || !is_unambiguous_http_path(path) {
            return None;
        }
        let method = Method::from_bytes(request.method(buffer)).ok()?;
        if self.challenge.is_some() && ChallengeStore::serves(path) {
            return None;
        }
        let route = self
            .service
            .select_route_path(Some(&authority), path, &method)
            .filter(|route| route.lane != LaneRoute::Classic)?;
        Some((route, authority))
    }

    async fn serve_request(
        &self,
        stream: &mut Stream,
        connection: &mut Http1Connection,
        shutdown: &ShutdownWatch,
        buffer: &[u8],
    ) -> Served {
        let mut scanned = buffers::take_head();
        let served = self
            .serve_scanned(stream, connection, shutdown, buffer, &mut scanned)
            .await;
        buffers::give_head(scanned);
        served
    }

    async fn serve_scanned(
        &self,
        stream: &mut Stream,
        connection: &mut Http1Connection,
        shutdown: &ShutdownWatch,
        buffer: &[u8],
        scanned: &mut RequestHead,
    ) -> Served {
        let Some(request) =
            head::scan(buffer, scanned).filter(|request| request.head_len == buffer.len())
        else {
            return Served::Handoff;
        };
        let Some((route, authority)) = self.select_route(buffer, &request) else {
            return Served::Handoff;
        };
        let started = Instant::now();
        connection.begin_request();
        connection.respect_request(request.close);
        if *shutdown.borrow() || self.generation.retired() {
            connection.disable_keepalive();
        }
        let generated = generated_request_id_bytes();
        let id = request
            .client_id
            .map_or(&generated[..], |span| span.of(buffer));
        if route.lane == LaneRoute::Proxy {
            let id = HeaderValue::from_bytes(id).expect("request ids are visible ASCII");
            let forward = proxy::Forward {
                buffer,
                head: scanned,
                request: &request,
                route: &route,
                id: &id,
                keepalive: connection.will_keepalive(),
            };
            return self
                .serve_proxied(stream, connection, forward, authority)
                .await;
        }
        let automatic = if connection.will_keepalive() {
            "keep-alive"
        } else {
            "close"
        };
        let HttpActionPlan::Fixed(response) = &route.action else {
            unreachable!("only direct routes reach the lane");
        };
        let mut out = BytesMut::new();
        direct::render(&mut out, response, id, Some(automatic), request.is_head);
        let written = tokio::time::timeout(self.write_budget, async {
            stream.write_all(&out).await?;
            stream.flush().await
        })
        .await;
        let sent = written.is_ok_and(|result| result.is_ok());
        let body = if request.is_head {
            0
        } else {
            response.body.len() as u64
        };
        let _ = self.metrics.record_bytes_sent(body);
        let status = sent.then_some(response.status);
        let _ = self
            .metrics
            .record_http_operation(HttpOperationResult::from_status(status), started.elapsed());
        connection.end_request();
        if sent && connection.will_keepalive() {
            return Served::Next;
        }
        let _ = stream.shutdown().await;
        Served::Close
    }
}

#[async_trait]
impl Http1Front for LaneFront {
    async fn serve(
        &self,
        mut stream: Stream,
        connection: &mut Http1Connection,
        shutdown: &ShutdownWatch,
    ) -> Http1FrontOutcome {
        loop {
            let Some(mut buffer) = self.wait_for_bytes(&mut stream, connection, shutdown).await
            else {
                return Http1FrontOutcome::Closed;
            };
            match self
                .serve_request(&mut stream, connection, shutdown, &buffer)
                .await
            {
                Served::Next => buffers::give_read(buffer),
                Served::Close => {
                    buffers::give_read(buffer);
                    return Http1FrontOutcome::Closed;
                }
                Served::Handoff => {
                    return Http1FrontOutcome::Handoff {
                        stream,
                        preloaded: buffer.split().freeze(),
                    };
                }
            }
        }
    }

    fn cleanup(&self) {
        self.first_request.notify_waiters();
    }
}
