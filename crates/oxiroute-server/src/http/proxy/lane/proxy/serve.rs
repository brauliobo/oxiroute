use std::{
    future::{Future as _, poll_fn},
    pin::pin,
    sync::Arc,
    task::Poll,
};

use http::uri::Authority;
use log::debug;
use pingora::{
    Error,
    apps::Http1Connection,
    connectors::http::v1::Connector,
    protocols::{Stream, http::v1::client::HttpSession},
};

use super::{Arrival, Forward, LOG_TARGET, Opened, Progress, Scanned, proxy_route, upstream_host};
use crate::{
    http::{
        evacuation::LeftRunning,
        proxy::{
            HttpRequestContext,
            attempt::{AttemptTimeouts, Exchanged},
            departure::{client_left, unless_gone},
            lane::{Served, end_connection, head::Target},
        },
    },
    upstream_peer::ReusePeer,
};

impl super::super::LaneFront {
    pub(in crate::http::proxy::lane) async fn serve_proxied(
        &self,
        stream: &mut Stream,
        connection: &mut Http1Connection,
        forward: Forward<'_>,
        authority: Authority,
    ) -> Served {
        debug!(
            target: LOG_TARGET,
            "served by the HTTP/1 lane: request_id={}",
            forward.id.to_str().unwrap_or_default()
        );
        let upstream = &*self.upstream;
        let target = forward
            .request
            .target(forward.buffer)
            .expect("the route was selected for this host");
        let counter = crate::clients::of_stream(stream);
        let mut ctx = self.begin(&forward, authority, &target, counter);
        let mut progress = Progress {
            received: 0,
            sent: 0,
            status: None,
            interim: false,
        };
        let outcome = self
            .exchange(
                stream,
                connection,
                &mut ctx,
                upstream,
                &forward,
                &mut progress,
            )
            .await;
        let error = outcome.err();
        if let Some(error) = &error {
            progress.status = self
                .fail(stream, &mut ctx, error, &forward, &mut progress)
                .await;
        }
        self.finish(&mut ctx, &forward, error.as_deref(), &progress);
        connection.end_request();
        if error.is_none() && connection.will_keepalive() {
            return Served::Next(ctx.now);
        }
        end_connection(stream).await;
        Served::Close
    }

    /// The request context of a request the lane serves, with the facts the classic request filter establishes.
    pub(in crate::http::proxy) fn begin(
        &self,
        forward: &Forward<'_>,
        authority: Authority,
        target: &Target<'_>,
        connection: Option<crate::clients::Counter>,
    ) -> Box<HttpRequestContext> {
        let (buffer, request, route) = (forward.buffer, forward.request, forward.route);
        let replay_safe = proxy_route(route).policy.request_is_replay_safe_with(
            forward.method,
            |name| {
                let name = name.as_str().as_bytes();
                let key = forward
                    .head
                    .headers()
                    .iter()
                    .find(|header| header.name(buffer).eq_ignore_ascii_case(name));
                key.is_some_and(|header| !header.value(buffer).is_empty())
            },
            request.body_len == 0,
            route.policy.request_buffering,
        );
        self.begin_arrival(Arrival {
            started: forward.started,
            route: (route, forward.id, authority),
            line: (
                forward.method,
                Some(target.host),
                target.path,
                forward.client_ip,
            ),
            fields: Scanned(buffer, forward.head),
            replay_safe,
            connection,
        })
    }

    /// The request context of a request the lane serves, whatever protocol it arrived by.
    pub(in crate::http::proxy) fn begin_arrival(
        &self,
        arrival: Arrival<'_>,
    ) -> Box<HttpRequestContext> {
        let Arrival {
            started,
            route: (route, id, authority),
            line,
            fields,
            replay_safe,
            connection,
        } = arrival;
        let proxy = proxy_route(route);
        // Boxed: the context is 1.9 kB and would be copied into every future that holds or returns it.
        let mut ctx = Box::new(HttpRequestContext::new(
            self.metrics.clone(),
            Some(Arc::clone(&self.generation)),
            started,
            self.service.upstream_io_timeout(),
        ));
        ctx.connection = connection;
        ctx.request_id = Some(id.clone());
        ctx.adopt_client_timeouts([self.timeouts.request, self.timeouts.session_write()]);
        ctx.register_routed_arrival(
            line,
            route.route_id.shared(),
            proxy.pool.label().shared(),
            self.service.access_log(),
        );
        ctx.authority = Some(authority);
        ctx.route = Some(Arc::clone(route));
        let policy = &proxy.policy;
        ctx.connection_retryable = policy.max_retries > 0;
        ctx.replay_safe = replay_safe;
        ctx.replay_retryable = policy.max_retries > 0 && replay_safe;
        ctx.evacuation_pin = LeftRunning::unmovable(false, replay_safe);
        if let Some(budget) = &policy.retry_budget {
            budget.record_request(ctx.now);
        }
        if proxy.pool.selector().schedules_queue() {
            ctx.queue_tag = proxy
                .pool
                .selector()
                .queue_tag(policy.queue_class.as_deref(), &fields);
        }
        ctx.adopt_route_total(
            policy
                .timeouts
                .total_ms
                .map(std::time::Duration::from_millis),
        );
        ctx.pool = Some(Arc::clone(&proxy.pool));
        ctx
    }

    /// The origin's answer, sent to the client; the status that went out is left in `progress`.
    async fn exchange(
        &self,
        stream: &mut Stream,
        connection: &mut Http1Connection,
        ctx: &mut HttpRequestContext,
        upstream: &Connector,
        forward: &Forward<'_>,
        progress: &mut Progress,
    ) -> Result<(), Box<Error>> {
        let opened = self
            .attempts(stream, ctx, upstream, forward, progress)
            .await?;
        progress.status = Some(opened.status);
        if !opened.keepalive {
            connection.disable_keepalive();
        }
        self.relay(stream, ctx, upstream, opened, progress)
            .await
            .map_err(|error| {
                let exchanged = Exchanged {
                    body_replayable: progress.received == 0,
                    response_retryable: false,
                    response_begun: true,
                };
                ctx.attempt_failed(error, exchanged)
            })
    }

    /// Attempts on the pool's servers until one answers or the request fails; an attempt an operator moved goes on
    /// to another server.
    async fn attempts(
        &self,
        stream: &mut Stream,
        ctx: &mut HttpRequestContext,
        upstream: &Connector,
        forward: &Forward<'_>,
        progress: &mut Progress,
    ) -> Result<Opened, Box<Error>> {
        loop {
            let mut cancelled = pin!(ctx.arm_evacuation_for_lane());
            // Polled in place: `select!` would move the attempt's future, which is kilobytes, into its own tuple.
            let raced = {
                let mut attempt = pin!(self.attempt(stream, ctx, upstream, forward, progress));
                poll_fn(|cx| match cancelled.as_mut().poll(cx) {
                    Poll::Ready(error) => Poll::Ready(Err(error)),
                    Poll::Pending => attempt.as_mut().poll(cx).map(Ok),
                })
                .await
            };
            let outcome = match raced {
                Ok(opened) => opened,
                Err(error) => Err(ctx.attempt_failed(error, Exchanged::UNANSWERED)),
            };
            match outcome {
                Ok(opened) => return Ok(opened),
                Err(error) if error.retry() => {}
                Err(error) => return Err(error),
            }
        }
    }

    async fn attempt(
        &self,
        stream: &mut Stream,
        ctx: &mut HttpRequestContext,
        upstream: &Connector,
        forward: &Forward<'_>,
        progress: &mut Progress,
    ) -> Result<Opened, Box<Error>> {
        // The client may leave while the request waits for a server or a connection: nothing has gone out yet.
        let timeouts = unless_gone(ctx.next_attempt(|_| None, upstream_host), || {
            client_left(stream)
        })
        .await?;
        let mut connection = match Self::idle_session(ctx, upstream).await {
            Some(idle) => idle,
            None => {
                unless_gone(Self::connect(ctx, upstream, timeouts), || {
                    client_left(stream)
                })
                .await?
            }
        };
        loop {
            let (session, reused, release) = connection;
            let sent = self
                .send_and_open(
                    stream,
                    ctx,
                    session,
                    (reused, release),
                    &timeouts,
                    forward,
                    progress,
                )
                .await;
            let exchanged = Exchanged {
                body_replayable: progress.received == 0,
                response_retryable: true,
                response_begun: progress.interim,
            };
            match sent {
                Err(error) if reused && ctx.closed_while_idle(&error, exchanged) => {
                    connection = Self::redial(stream, ctx, upstream, timeouts).await?;
                }
                sent => return sent.map_err(|error| ctx.attempt_failed(error, exchanged)),
            }
        }
    }

    async fn redial(
        stream: &mut Stream,
        ctx: &mut HttpRequestContext,
        upstream: &Connector,
        timeouts: AttemptTimeouts,
    ) -> Result<(Box<HttpSession>, bool, ReusePeer), Box<Error>> {
        ctx.reselect_attempted()
            .await
            .map_err(|error| ctx.connect_failed(error.into_up()))?;
        unless_gone(Self::connect(ctx, upstream, timeouts), || {
            client_left(stream)
        })
        .await
    }

    /// An idle connection to the selected server, when its address is fixed: no peer is built to ask for one.
    pub(in crate::http::proxy) async fn idle_session(
        ctx: &HttpRequestContext,
        upstream: &Connector,
    ) -> Option<(Box<HttpSession>, bool, ReusePeer)> {
        let peer = ctx.reuse_peer()?;
        let session = upstream.reused_boxed_http_session(&peer).await?;
        Some((session, true, peer))
    }

    /// A session on a new connection, or on an idle one for a server whose address is resolved per attempt.
    pub(in crate::http::proxy) async fn connect(
        ctx: &mut HttpRequestContext,
        upstream: &Connector,
        timeouts: AttemptTimeouts,
    ) -> Result<(Box<HttpSession>, bool, ReusePeer), Box<Error>> {
        let peer = ctx.prepare_peer(timeouts).await?;
        match upstream.get_http_session(&*peer).await {
            Ok((session, reused)) => Ok((Box::new(session), reused, ReusePeer::of(&peer))),
            Err(mut error) => {
                error.as_up();
                Err(ctx.connect_failed(error).into_up())
            }
        }
    }
}
