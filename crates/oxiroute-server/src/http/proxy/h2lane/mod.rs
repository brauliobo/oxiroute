//! The HTTP/2 fast lane, the sibling of the HTTP/1 lane (`lane`): an HTTP/2 request of the common shape (no body, a
//! fixed response or a plain proxy route, see `LaneRoute`) is served on the stream itself, before any Pingora
//! session, `RequestHeader` of the proxy or hook exists. The accept loop of the vendored core asks the lane
//! (`Http2Front`) for each request that plainly passes its own acceptance checks, as the decoded head of the connection; the lane claims it or hands it back for
//! the classic session, which stays the semantic reference.
//!
//! * `claim` decides, without waiting, and reads the request (`request` is the only module that knows how it is
//!   stored);
//! * `head` writes the HTTP/1 request head the origin gets, `response` turns the origin's answer into the HTTP/2
//!   response, `direct` serves fixed responses, `exchange` runs the proxied exchange with the classic request
//!   context's steps, `relay` moves the body with flow control and watches the client.

mod claim;
mod direct;
mod exchange;
mod fast;
mod head;
mod relay;
mod request;
mod response;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use pingora::{
    apps::{Http2Claim, Http2Front},
    protocols::{
        Digest,
        http::v2::server::{H2Request, H2Stream},
    },
    server::ShutdownWatch,
};

use super::lane::LaneFront;

impl Http2Front for LaneFront {
    fn claim(
        self: Arc<Self>,
        request: H2Request,
        stream: H2Stream,
        digest: &Digest,
        _shutdown: &ShutdownWatch,
    ) -> Http2Claim {
        claim::claim(self, request, stream, digest)
    }
}
