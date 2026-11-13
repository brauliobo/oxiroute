//! The compiled fast lane: HTTP/1 requests of the common shape are served on the connection itself, before any
//! Pingora session exists. A request the lane cannot prove equivalent to the classic path (a head it does not
//! fully understand, a route that needs a hook, a body, an upgrade, pipelined bytes) is handed to the classic path
//! with the bytes already read, so the classic path stays the semantic reference and nothing is read twice.
//!
//! The lane exists for requests whose cost is the layers around them; it never grows the grammar, it refuses a
//! superset of what the classic parser refuses.

pub(in crate::http::proxy) mod buffers;
mod direct;
pub(in crate::http::proxy) mod head;
mod local;
pub(in crate::http::proxy) mod log;
mod peek;
mod plan;
pub(in crate::http::proxy) mod proxy;
pub(in crate::http::proxy) mod render;
pub(in crate::http::proxy) mod response;
pub(in crate::http::proxy) mod rules;
mod stop;
pub(in crate::http::proxy) mod timing;
mod upload;

use std::{
    future::{Future, poll_fn},
    pin::{Pin, pin},
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use http::{HeaderValue, Method, uri::Authority};
use oxiroute_acme::ChallengeStore;
use oxiroute_h1_head::RequestHead;
use pingora::{
    apps::{
        Http1Connection, Http1Front, Http1FrontOutcome, PooledFuture, relocate_idle_connection,
    },
    connectors::http::v1::Connector,
    protocols::Stream,
    server::ShutdownWatch,
};
use tokio::{io::AsyncWriteExt as _, sync::Notify};

use stop::Stop;
use timing::{Wait, within};

pub(crate) use plan::LaneRoute;
pub(crate) use rules::RouteEdits;

use crate::{
    HttpServicePlan, ListenerMetrics, RuntimeGeneration,
    http::{
        action::{CachedFile, HttpRoutePlan},
        request_id::generated_request_id_bytes,
    },
};

use local::{Exchange, Local};

/// The route a request is served by, the authority it was selected for and, for a static route, the file the cache
/// holds for it.
type Selected = (Arc<HttpRoutePlan>, Authority, Option<Arc<CachedFile>>);

/// The downstream timeouts of the listener, as the classic path applies them to a session.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LaneTimeouts {
    pub(crate) request: Option<Duration>,
    pub(crate) write: Option<Duration>,
    pub(crate) keepalive: Option<Duration>,
}

impl LaneTimeouts {
    /// The write timeout a session has: the request timeout caps it.
    pub(in crate::http::proxy) fn session_write(&self) -> Option<Duration> {
        match (self.request, self.write) {
            (Some(request), Some(write)) => Some(request.min(write)),
            (request, None) => request,
            (None, write) => write,
        }
    }
}

pub(crate) struct LaneFront {
    pub(in crate::http::proxy) challenge: Option<ChallengeStore>,
    first_request: Notify,
    pub(in crate::http::proxy) generation: Arc<RuntimeGeneration>,
    pub(in crate::http::proxy) metrics: ListenerMetrics,
    pub(in crate::http::proxy) service: Arc<HttpServicePlan>,
    pub(in crate::http::proxy) timeouts: LaneTimeouts,
    /// The upstream connector of the classic proxy, whose idle connections the proxied lane shares.
    pub(in crate::http::proxy) upstream: Arc<Connector>,
    /// The service's name as its access lines write it, with the field's key.
    pub(in crate::http::proxy) service_log: Box<[u8]>,
    /// Time a response write may take before the connection is given up: the least of the service deadline and
    /// the downstream timeouts.
    pub(in crate::http::proxy) write_budget: Duration,
}

/// Ends a connection after its last response. TLS says goodbye; a plain socket only flushes, because the close that
/// follows sends the FIN after the queued bytes, as a `shutdown` before it would have.
pub(in crate::http::proxy) async fn end_connection(stream: &mut Stream) {
    let _ = if stream.get_ssl().is_some() {
        stream.shutdown().await
    } else {
        stream.flush().await
    };
}

/// The output of `work`, or `None` once `stopped` says so; `work` is polled first.
async fn raced<T>(
    mut work: Pin<&mut impl Future<Output = Option<T>>>,
    mut stopped: impl FnMut(&mut Context<'_>) -> bool,
) -> Option<T> {
    poll_fn(|cx| {
        if let Poll::Ready(output) = work.as_mut().poll(cx) {
            return Poll::Ready(output);
        }
        if stopped(cx) {
            Poll::Ready(None)
        } else {
            Poll::Pending
        }
    })
    .await
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
    /// `idle_since` is when the wait began; `stop` resolves when the service stops, and is polled by the waits
    /// of every request of the connection, so it registers once and not once per wait.
    async fn wait_for_bytes(
        &self,
        stream: &mut Stream,
        connection: &Http1Connection,
        stop: &mut Stop<'_, impl Future<Output = ()>>,
        idle_since: Instant,
    ) -> Option<BytesMut> {
        // Between two requests nothing of the connection is parked: it may move to a lighter worker (worker_scaling).
        if connection.is_reused() {
            if relocate_idle_connection(stream).await.is_err() {
                return None;
            }
        } else if let Some(buffer) = buffers::read_arrived(stream) {
            return Some(buffer);
        }
        let mut read = pin!(buffers::read_request(stream));
        // A read that is ready at once needs no timer.
        let deadline = self
            .idle_timeout(connection, stop.stopping())
            .and_then(|limit| idle_since.checked_add(limit));
        let mut timed = pin!(async {
            match deadline {
                Some(deadline) => within(
                    Wait::Idle,
                    tokio::time::Instant::from_std(deadline),
                    read.as_mut(),
                )
                .await
                .unwrap_or(None),
                None => read.as_mut().await,
            }
        });
        // A connection waiting between requests ends with the signal its owner shares; the first request of a
        // connection ends when the service stops.
        if connection.is_reused() {
            raced(timed.as_mut(), |cx| stop.poll(cx).is_ready()).await
        } else {
            let mut first = pin!(self.first_request.notified());
            raced(timed.as_mut(), |cx| first.as_mut().poll(cx).is_ready()).await
        }
    }

    /// The route the request is served by, when the lane may serve it, and the authority it was selected for. The
    /// route chosen from the first lines stands when the scanned head has the host it was chosen for.
    fn select_route(
        &self,
        buffer: &[u8],
        request: &head::Request,
        early: peek::Early<'_>,
    ) -> Option<Selected> {
        let path = request.path(buffer);
        let authority = head::authority(request.host(buffer)?)?;
        if authority.as_str().contains('@') {
            return None;
        }
        if self.challenge.is_some() && ChallengeStore::serves(path) {
            return None;
        }
        let (route, file) = if let Some(selected) = early.route(buffer, request) {
            selected
        } else {
            let method = Method::from_bytes(request.method(buffer)).ok()?;
            let route = self
                .service
                .select_route_path(Some(authority.as_str()), path, &method)
                .filter(|route| route.lane != LaneRoute::Classic)?;
            let file = peek::cached_file(route, path, &method).ok()?;
            (Arc::clone(route), file)
        };
        Some((route, authority, file))
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
        (stopping, client_ip): (bool, Option<std::net::IpAddr>),
        buffer: &[u8],
    ) -> Served {
        let mut scanned = buffers::take_head();
        let served = self
            .serve_scanned(
                stream,
                connection,
                (stopping, client_ip),
                buffer,
                &mut scanned,
            )
            .await;
        buffers::give_head(scanned);
        served
    }

    #[allow(clippy::too_many_lines)]
    async fn serve_scanned(
        &self,
        stream: &mut Stream,
        connection: &mut Http1Connection,
        (stopping, client_ip): (bool, Option<std::net::IpAddr>),
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
        let Some((route, authority, file)) = self.select_route(buffer, &request, early) else {
            return Served::Handoff;
        };
        if file.is_some() && head::is_conditional(scanned, buffer) {
            return Served::Handoff;
        }
        if request.body_len != 0 && !Self::takes_body(&route, &request) {
            return Served::Handoff;
        }
        let values = if route.lane == LaneRoute::Proxy {
            let Some(values) = proxy::decide(
                stream.get_ssl().is_some(),
                &route,
                (buffer, client_ip),
                scanned,
                &authority,
            ) else {
                return Served::Handoff;
            };
            values
        } else {
            rules::Evaluated::default()
        };
        let started = Instant::now();
        connection.begin_request();
        connection.respect_request(request.close);
        if stopping || self.generation.retired() {
            connection.disable_keepalive();
        }
        let generated = generated_request_id_bytes();
        if route.lane == LaneRoute::Proxy {
            // A generated id is owned once and shared by the clones the request context makes of it.
            let id = match request.client_id {
                Some(span) => HeaderValue::from_bytes(span.of(buffer)),
                None => HeaderValue::from_maybe_shared(Bytes::from_owner(generated)),
            }
            .expect("request ids are visible ASCII");
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
        let id = request
            .client_id
            .map_or(&generated[..], |span| span.of(buffer));
        let exchange = Exchange {
            route: &route,
            buffer,
            request: &request,
            authority: &authority,
            id,
            started,
        };
        let local = match (&file, route.action.direct_response()) {
            (Some(file), _) => Local::File(file),
            (None, Some(response)) => Local::Direct(response),
            (None, None) => unreachable!("only direct and cached static routes reach this point"),
        };
        self.serve_local(stream, connection, exchange, local).await
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
        let stopped = pin!(async {
            let _ = watch.wait_for(|stopping| *stopping).await;
        });
        let mut stop = Stop::new(shutdown, stopped);
        // The peer of a connection does not change.
        let client_ip = proxy::client_ip(&stream);
        let mut idle_since = Instant::now();
        loop {
            let Some(mut buffer) = self
                .wait_for_bytes(&mut stream, connection, &mut stop, idle_since)
                .await
            else {
                return Http1FrontOutcome::Closed;
            };
            // On the heap, in a block the thread keeps for the next request: the connection's own future, which is
            // all an idle connection holds, stays small instead of containing the state of serving a request.
            let served = {
                let (stream, connection, buffer) = (&mut stream, &mut *connection, &buffer);
                let context = (stop.stopping(), client_ip);
                PooledFuture::new(move || self.serve_request(stream, connection, context, buffer))
            }
            .await;
            match served {
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
