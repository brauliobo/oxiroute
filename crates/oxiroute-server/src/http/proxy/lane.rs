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
mod log;
mod peek;
mod plan;
mod proxy;
mod response;
mod rules;
mod timing;

use std::{
    future::{Future, poll_fn},
    pin::{Pin, pin},
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

use timing::within;

pub(crate) use plan::LaneRoute;
pub(crate) use rules::RouteEdits;

use crate::{
    HttpOperationResult, HttpServicePlan, ListenerMetrics, RuntimeGeneration,
    http::{action::HttpRoutePlan, request_id::generated_request_id_bytes},
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
    /// The service's name as its access lines write it, with the field's key.
    service_log: Box<[u8]>,
    /// Time a response write may take before the connection is given up: the least of the service deadline and
    /// the downstream timeouts.
    write_budget: Duration,
}

enum Served {
    /// The connection serves another request; the clock reading at which this one ended, from which its idle wait
    /// is timed.
    Next(Instant),
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
            && service.automatic_response_headers()
            && service.has_lane_route();
        let service_log = service
            .access_log()
            .map(|log| log::service_part(log))
            .unwrap_or_default();
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
            service_log,
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
    /// `idle_since` is when the wait began; `stopped` resolves when the service stops, and is polled by the waits
    /// of every request of the connection, so it registers once and not once per wait.
    async fn wait_for_bytes(
        &self,
        stream: &mut Stream,
        connection: &Http1Connection,
        stopping: bool,
        idle_since: Instant,
        stopped: Pin<&mut impl Future<Output = ()>>,
    ) -> Option<BytesMut> {
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
        // A read that is ready at once needs no timer.
        let timed = async {
            let deadline = self
                .idle_timeout(connection, stopping)
                .and_then(|limit| idle_since.checked_add(limit));
            match deadline {
                Some(deadline) => within(tokio::time::Instant::from_std(deadline), read)
                    .await
                    .unwrap_or(None),
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
                    stopped.await;
                } else {
                    self.first_request.notified().await;
                }
            } => None,
        }
    }

    /// The route the request is served by, when the lane may serve it, and the authority it was selected for. The
    /// route chosen from the first lines stands when the scanned head has the host it was chosen for.
    fn select_route(
        &self,
        buffer: &[u8],
        request: &head::Request,
        early: peek::Early<'_>,
    ) -> Option<(Arc<HttpRoutePlan>, Authority)> {
        let path = request.path(buffer);
        let authority = request.host(buffer)?.parse::<Authority>().ok()?;
        if authority.as_str().contains('@') || !is_unambiguous_http_path(path) {
            return None;
        }
        if self.challenge.is_some() && ChallengeStore::serves(path) {
            return None;
        }
        let route = if let Some(route) = early.route(buffer, request) {
            route
        } else {
            let method = Method::from_bytes(request.method(buffer)).ok()?;
            self.service
                .select_route_path(Some(&authority), path, &method)
                .filter(|route| route.lane != LaneRoute::Classic)?
        };
        Some((route, authority))
    }

    /// Whether the lane forwards the body of the request to its route: only a proxied route does, and a length over
    /// the route's limit is the classic path's `413`.
    fn takes_body(route: &HttpRoutePlan, request: &head::Request) -> bool {
        route.lane == LaneRoute::Proxy && !route.policy.exceeds_body_limit(request.body_len as u64)
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

    #[allow(clippy::too_many_lines)]
    async fn serve_scanned(
        &self,
        stream: &mut Stream,
        connection: &mut Http1Connection,
        shutdown: &ShutdownWatch,
        buffer: &[u8],
        scanned: &mut RequestHead,
    ) -> Served {
        let Some(early) = peek::decide(&self.service, buffer) else {
            return Served::Handoff;
        };
        let Some(request) =
            head::scan(buffer, early.line, scanned).filter(|request| request.admits(buffer.len()))
        else {
            return Served::Handoff;
        };
        let Some((route, authority)) = self.select_route(buffer, &request, early) else {
            return Served::Handoff;
        };
        if request.body_len != 0 && !Self::takes_body(&route, &request) {
            return Served::Handoff;
        }
        let client_ip = if route.lane == LaneRoute::Proxy {
            proxy::client_ip(stream)
        } else {
            None
        };
        let values = if route.lane == LaneRoute::Proxy {
            let Some(values) =
                proxy::decide(stream, &route, (buffer, client_ip), scanned, &authority)
            else {
                return Served::Handoff;
            };
            values
        } else {
            rules::Evaluated::default()
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
            let method = Method::from_bytes(request.method(buffer))
                .expect("the route was selected for this method");
            let forward = proxy::Forward {
                started,
                buffer,
                method: &method,
                client_ip,
                head: scanned,
                request: &request,
                route: &route,
                id: &id,
                values: &values,
                keepalive: connection.will_keepalive(),
            };
            let served = self
                .serve_proxied(stream, connection, forward, authority)
                .await;
            buffers::give_write(values.into_buffer());
            return served;
        }
        let automatic = if connection.will_keepalive() {
            "keep-alive"
        } else {
            "close"
        };
        let Some(response) = route.action.direct_response() else {
            unreachable!("only direct routes reach the lane");
        };
        let mut out = BytesMut::new();
        direct::render(&mut out, response, id, Some(automatic), request.is_head);
        let written = tokio::time::timeout(self.write_budget, async {
            stream.write_all(&out).await?;
            stream.flush().await
        })
        .await;
        let ended = Instant::now();
        let sent = written.is_ok_and(|result| result.is_ok());
        let body = if request.is_head {
            0
        } else {
            response.body.len() as u64
        };
        let _ = self.metrics.record_bytes_sent(body);
        let status = sent.then_some(response.status);
        let _ = self.metrics.record_http_operation(
            HttpOperationResult::from_status(status),
            ended.saturating_duration_since(started),
        );
        if let Some(log) = self.service.access_log() {
            let method = Method::from_bytes(request.method(buffer))
                .expect("the route was selected for this method");
            let line = log::Line {
                attempts: 0,
                received: 0,
                sent: body,
                connect: None,
                header: None,
                id,
                id_generated: request.client_id.is_none(),
                elapsed: ended.saturating_duration_since(started),
                evacuations: 0,
                host: Some(&authority),
                idempotency: None,
                method: &method,
                queue_wait: Duration::ZERO,
                reason: "",
                rescued: false,
                servers: &[],
                status,
                unix_ms: crate::clock::wall_ms(ended),
                stuck: false,
            };
            log::write(log, &self.service_log, &route.edits.log, &line);
        }
        connection.end_request();
        if sent && connection.will_keepalive() {
            return Served::Next(ended);
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
        let mut watch = shutdown.clone();
        let stopped = async {
            let _ = watch.wait_for(|stopping| *stopping).await;
        };
        let mut stopped = pin!(stopped);
        let mut idle_since = Instant::now();
        loop {
            let stopping = *shutdown.borrow();
            let Some(mut buffer) = self
                .wait_for_bytes(
                    &mut stream,
                    connection,
                    stopping,
                    idle_since,
                    stopped.as_mut(),
                )
                .await
            else {
                return Http1FrontOutcome::Closed;
            };
            match self
                .serve_request(&mut stream, connection, shutdown, &buffer)
                .await
            {
                Served::Next(ended) => {
                    buffers::give_read(buffer);
                    idle_since = ended;
                }
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
