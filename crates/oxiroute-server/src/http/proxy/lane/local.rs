//! Responses the lane writes from what the route holds, with no upstream: a fixed response, or a file the static
//! cache answers. Both end the same way: metrics, the access line, and the connection's next step.

use std::{future::Future, io, time::Instant};

use bytes::BytesMut;
use http::{Method, uri::Authority};
use pingora::{
    apps::Http1Connection, protocols::Stream, protocols::l4::sendfile::send_file_to_stream,
};
use tokio::io::AsyncWriteExt as _;

use super::{LaneFront, Served, direct, head, log};
use crate::{
    HttpOperationResult,
    http::action::{CachedFile, FixedResponsePlan, HttpRoutePlan, StaticContent},
};

pub(super) enum Local<'a> {
    Direct(&'a FixedResponsePlan),
    File(&'a CachedFile),
}

/// The request a local response answers.
pub(super) struct Exchange<'a> {
    pub(super) route: &'a HttpRoutePlan,
    pub(super) buffer: &'a [u8],
    pub(super) request: &'a head::Request,
    pub(super) authority: &'a Authority,
    pub(super) id: &'a [u8],
    pub(super) started: Instant,
}

impl LaneFront {
    pub(super) async fn serve_local(
        &self,
        stream: &mut Stream,
        connection: &mut Http1Connection,
        exchange: Exchange<'_>,
        local: Local<'_>,
    ) -> Served {
        let automatic = if connection.will_keepalive() {
            "keep-alive"
        } else {
            "close"
        };
        let id = exchange.id;
        let head = exchange.request.is_head;
        let (sent, status, body) = match local {
            Local::Direct(response) => {
                let mut out = BytesMut::new();
                direct::render(&mut out, response, id, Some(automatic), head);
                let sent = self.within_budget(write_all_flushed(stream, &out)).await;
                (sent, response.status, response.body.len() as u64)
            }
            Local::File(cached) => {
                let sent = self.write_file(stream, cached, id, automatic, head).await;
                (sent, 200, cached.file.size)
            }
        };
        let body = if head { 0 } else { body };
        self.conclude(stream, connection, exchange, sent, status, body)
            .await
    }

    async fn conclude(
        &self,
        stream: &mut Stream,
        connection: &mut Http1Connection,
        exchange: Exchange<'_>,
        sent: bool,
        status: u16,
        body: u64,
    ) -> Served {
        let Exchange {
            route,
            buffer,
            request,
            authority,
            id,
            started,
        } = exchange;
        let ended = Instant::now();
        let _ = self.metrics.record_bytes_sent(body);
        let status = sent.then_some(status);
        let _ = self.metrics.record_http_operation(
            HttpOperationResult::from_status(status),
            ended.saturating_duration_since(started),
        );
        if let Some(log) = self.service.access_log() {
            let method = Method::from_bytes(request.method(buffer))
                .expect("the route was selected for this method");
            let line = log::Line::answered(
                (id, request.client_id.is_none()),
                (authority, &method),
                (status, body),
                (started, ended),
                log::HTTP1,
            );
            log::write(log, &self.service_log, &route.edits.log, &line);
        }
        connection.end_request();
        if sent && connection.will_keepalive() {
            return Served::Next(ended);
        }
        super::end_connection(stream).await;
        Served::Close
    }

    /// Whether `operation` finished, successfully, inside the time a response write may take.
    async fn within_budget<T>(&self, operation: impl Future<Output = io::Result<T>>) -> bool {
        tokio::time::timeout(self.write_budget, operation)
            .await
            .is_ok_and(|result| result.is_ok())
    }

    /// The response of a cached file: the head the entry rendered, then the body, which a small file has in memory
    /// and a large one sends from its descriptor.
    async fn write_file(
        &self,
        stream: &mut Stream,
        cached: &CachedFile,
        id: &[u8],
        automatic: &str,
        head: bool,
    ) -> bool {
        let rendered = cached.head.as_deref().unwrap_or_default();
        let mut out = BytesMut::with_capacity(rendered.len() + id.len() + 128);
        direct::put_head(&mut out, rendered, id, Some(automatic));
        let file = &cached.file;
        match &file.content {
            StaticContent::Memory(body) => {
                let body = if head { &[][..] } else { &body[..] };
                self.within_budget(async {
                    stream.write_all(&out).await?;
                    stream.write_all(body).await?;
                    stream.flush().await
                })
                .await
            }
            StaticContent::Open { .. } if head || file.size == 0 => {
                self.within_budget(write_all_flushed(stream, &out)).await
            }
            StaticContent::Open { .. } => {
                self.within_budget(write_all_flushed(stream, &out)).await
                    && self
                        .send_body(stream, file.content.clone(), file.size)
                        .await
            }
        }
    }

    /// The bytes of an open file: the kernel sends what the page cache holds and the connection can take, the chunk
    /// reader the rest. Every write has the time a response write may take.
    async fn send_body(&self, stream: &mut Stream, content: StaticContent, size: u64) -> bool {
        let mut reader = content.into_reader(0, size);
        while let Some(slice) = reader.kernel_slice() {
            let sent = tokio::time::timeout(
                self.write_budget,
                send_file_to_stream(stream, &slice.file, slice.offset, slice.length),
            )
            .await;
            match sent {
                Ok(Ok(Some(sent))) if sent == slice.length => reader.advance(sent as u64),
                Ok(Ok(None)) => reader.without_kernel(),
                _ => return false,
            }
        }
        while let Ok(Some(chunk)) = reader.next_chunk().await {
            if !self.within_budget(stream.write_all(&chunk)).await {
                return false;
            }
        }
        reader.is_done() && self.within_budget(stream.flush()).await
    }
}

async fn write_all_flushed(stream: &mut Stream, bytes: &[u8]) -> io::Result<()> {
    stream.write_all(bytes).await?;
    stream.flush().await
}
