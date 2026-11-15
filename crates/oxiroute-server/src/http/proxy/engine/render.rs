use std::time::Instant;

use oxiroute_epoll_h1::{Request, Response};

use super::exchange::Exchange;
use crate::http::proxy::lane::{render::render_request, response::render_head};

impl Exchange {
    pub(super) fn render_request(&self, request: &Request<'_>, out: &mut Vec<u8>) {
        let forward = self.forward(request.bytes(), request.head());
        render_request(out, &forward, &self.host, self.connection_close);
    }

    pub(super) fn render_response(&self, response: &Response<'_>, now: Instant, out: &mut Vec<u8>) {
        render_head(
            out,
            response.bytes,
            response.head,
            (response.request_is_head, self.route.as_ref(), &self.id),
            response.keepalive,
            (now, self.ctx.queue_stamp()),
        );
    }
}
