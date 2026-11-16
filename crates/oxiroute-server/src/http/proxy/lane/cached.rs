//! A fresh hit of a cached route, answered on the connection. The cache decides, with the key, `Vary`, the request's
//! cache directives and the entry's freshness it has for the classic path; the lane takes the answer only when the
//! cache says the stored response is the answer as it is. A request with a condition or a range, a miss, a stale or
//! revalidated entry, and every request the cache or the route does not accept are the classic path's, which answers
//! them and fills the cache.

mod wire;

use std::{net::IpAddr, time::Instant};

use bytes::{BufMut as _, BytesMut};
use http::{Method, uri::Authority};
use oxiroute_cache::{FreshHit, RequestKeyInput};
use oxiroute_h1_head::RequestHead;
use pingora::{apps::Http1Connection, protocols::Stream};
use tokio::io::AsyncWriteExt as _;

use super::{LaneFront, Served, buffers, head as request_head, local::Exchange, proxy};
use crate::{
    http::{
        action::{HttpActionPlan, HttpRoutePlan},
        cache::HttpCachePlan,
        request_id::{REQUEST_ID_HEADER, UPSTREAM_REQUEST_ID_HEADER, generated_request_id_bytes},
    },
    observability::monitoring::CacheEvent,
};

/// The log target of the debug line that says a hit was served by the lane, with the id of its request: how a test
/// (or an operator who raises the level of this target) tells the lane from the classic path.
pub(crate) const LOG_TARGET: &str = "oxiroute::h1lane";

/// A body this small is written with the head, in one buffer.
const INLINE_BODY: usize = 16 * 1024;

/// The request of a cached route, as the lane read it.
pub(super) struct Cached<'a> {
    pub(super) route: &'a HttpRoutePlan,
    pub(super) buffer: &'a [u8],
    pub(super) head: &'a RequestHead,
    pub(super) request: &'a request_head::Request,
    pub(super) authority: &'a Authority,
    pub(super) https: bool,
}

impl Cached<'_> {
    fn plan(&self) -> &HttpCachePlan {
        let HttpActionPlan::Proxy(proxy) = &self.route.action else {
            unreachable!("only proxied routes have a cache");
        };
        proxy
            .policy
            .cache
            .as_deref()
            .expect("a cached route has a cache plan")
    }

    /// The hit the lane answers, already counted as served; `None` leaves the request to the classic path with the
    /// cache as it was.
    fn answer(&self) -> Option<FreshHit> {
        let request = self.request;
        let method = Method::from_bytes(request.method(self.buffer)).ok()?;
        let plan = self.plan();
        let servable = matches!(method, Method::GET | Method::HEAD)
            && plan.allows_method(&method)
            && request.body_len == 0
            && !request_head::is_conditional(self.head, self.buffer);
        if !servable {
            return None;
        }
        let headers = request_head::header_map(self.head, self.buffer)?;
        let cache = plan.cache.memory()?;
        let hit = cache.fresh_hit(RequestKeyInput {
            method: &method,
            scheme: if self.https { "https" } else { "http" },
            authority: self.authority.as_str(),
            path: request.path(self.buffer),
            query: request.query(self.buffer),
            headers: &headers,
        })?;
        let stamped = hit.headers().contains_key(REQUEST_ID_HEADER)
            || hit.headers().contains_key(UPSTREAM_REQUEST_ID_HEADER);
        if stamped {
            return None;
        }
        cache.record_hit(&hit);
        Some(hit)
    }
}

impl LaneFront {
    pub(super) async fn serve_cached(
        &self,
        stream: &mut Stream,
        connection: &mut Http1Connection,
        (stopping, client_ip): (bool, Option<IpAddr>),
        cached: Cached<'_>,
    ) -> Served {
        let Some(values) = proxy::decide(
            cached.https,
            cached.route,
            (cached.buffer, client_ip),
            cached.head,
            cached.authority,
        ) else {
            return Served::Handoff;
        };
        buffers::give_write(values.into_buffer());
        let Some(hit) = cached.answer() else {
            return Served::Handoff;
        };
        let _ = self.metrics.record_cache_event(CacheEvent::Hit);
        let started = Instant::now();
        connection.begin_request();
        connection.respect_request(cached.request.close);
        if stopping || self.generation.retired() {
            connection.disable_keepalive();
        }
        let generated = generated_request_id_bytes();
        let id = cached
            .request
            .client_id
            .map_or(&generated[..], |span| span.of(cached.buffer));
        log::debug!(
            target: LOG_TARGET,
            "served by the HTTP/1 lane: request_id={}",
            String::from_utf8_lossy(id)
        );
        let exchange = Exchange {
            route: cached.route,
            buffer: cached.buffer,
            request: cached.request,
            authority: cached.authority,
            id,
            started,
        };
        self.write_hit(stream, connection, exchange, &hit).await
    }

    async fn write_hit(
        &self,
        stream: &mut Stream,
        connection: &mut Http1Connection,
        exchange: Exchange<'_>,
        hit: &FreshHit,
    ) -> Served {
        let automatic = if connection.will_keepalive() {
            "keep-alive"
        } else {
            "close"
        };
        let body = wire::body_sent(hit, exchange.request.is_head);
        let inline = body.len() <= INLINE_BODY;
        let mut out = BytesMut::with_capacity(512 + if inline { body.len() } else { 0 });
        wire::put(&mut out, hit, (exchange.id, automatic));
        if inline {
            out.put_slice(body);
        }
        let sent = self
            .within_budget(async {
                stream.write_all(&out).await?;
                if !inline {
                    stream.write_all(body).await?;
                }
                stream.flush().await
            })
            .await;
        let (status, sent_bytes) = (hit.status().as_u16(), body.len() as u64);
        self.conclude(stream, connection, exchange, sent, status, sent_bytes)
            .await
    }
}
