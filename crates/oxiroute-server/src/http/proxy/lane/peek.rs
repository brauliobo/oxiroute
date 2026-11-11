//! The route of a request, decided from its request line (and its `Host` field when the routes depend on the host)
//! before the header fields are scanned. A request whose route the lane does not serve goes to the classic path at
//! once: scanning its fields first would parse a head that the session parses again. The line is read once; the scan
//! of a request that may be the lane's continues after it, and keeps the route that was selected.
//!
//! The decision only ever shortcuts to the classic path, which is the reference: whatever the line or the host say
//! that the lane cannot use, the classic path reads the request.

use std::{sync::Arc, time::Instant};

use http::{Method, uri::Authority};

use super::{LaneRoute, head};
use crate::{
    HttpServicePlan,
    http::action::{CachedFile, HttpRoutePlan},
};

/// A request the lane may serve, as far as its first lines say.
pub(super) struct Early<'a> {
    pub(super) line: head::Line,
    /// The route selected for the request line and host, when the lines were enough to select it, and the file of a
    /// static route that the cache holds.
    route: Option<(Arc<HttpRoutePlan>, Option<Arc<CachedFile>>)>,
    /// The host the route was selected for, when the host takes part in the selection.
    host: Option<&'a str>,
}

/// `None` when the classic path serves the request: its line is partial or not the lane's, or its route is.
pub(super) fn decide<'a>(service: &HttpServicePlan, buffer: &'a [u8]) -> Option<Early<'a>> {
    let line = head::line(buffer)?;
    let host = service.selects_on_host().then(|| host_of(buffer, &line));
    let authority = match host {
        Some(host) => match host.and_then(|host| host.parse::<Authority>().ok()) {
            Some(authority) => Some(authority),
            None => {
                return Some(Early {
                    line,
                    route: None,
                    host: None,
                });
            }
        },
        None => None,
    };
    let method = Method::from_bytes(line.method(buffer)).ok()?;
    let route = service
        .select_route_path(authority.as_ref(), line.path(buffer), &method)
        .filter(|route| route.lane != LaneRoute::Classic)?;
    let file = cached_file(&route, line.path(buffer), &method).ok()?;
    Some(Early {
        line,
        route: Some((route, file)),
        host: host.flatten(),
    })
}

/// The classic path serves the request: the cache does not hold the file or the lane does not serve the method.
pub(super) struct Classic;

/// What the cache holds for the request of a static route: nothing for a route that is not static, the file the
/// cache answers with otherwise. A miss costs one lookup, not a scan of the head, and the classic path fills the
/// cache.
pub(super) fn cached_file(
    route: &HttpRoutePlan,
    path: &str,
    method: &Method,
) -> Result<Option<Arc<CachedFile>>, Classic> {
    let Some(files) = route
        .action
        .static_files()
        .filter(|_| route.lane == LaneRoute::Static)
    else {
        return Ok(None);
    };
    if method != Method::GET && method != Method::HEAD {
        return Err(Classic);
    }
    files
        .cached(path, Instant::now())
        .filter(|cached| cached.head.is_some())
        .map(Some)
        .ok_or(Classic)
}

impl Early<'_> {
    /// The route selected from the first lines, when the scanned head has the host it was selected for.
    pub(super) fn route(
        self,
        buffer: &[u8],
        request: &head::Request,
    ) -> Option<(Arc<HttpRoutePlan>, Option<Arc<CachedFile>>)> {
        let host = self.host;
        self.route
            .filter(|_| host.is_none_or(|host| Some(host) == request.host(buffer)))
    }
}

/// The value of the first `Host` field after the request line, for a head whose fields are not scanned yet.
fn host_of<'a>(buffer: &'a [u8], line: &head::Line) -> Option<&'a str> {
    let fields = buffer.get(line.fields_start()..)?;
    for field in fields.split(|byte| *byte == b'\n') {
        let field = field.strip_suffix(b"\r")?;
        let (name, value) = field.split_at(field.iter().position(|byte| *byte == b':')?);
        if name.eq_ignore_ascii_case(b"host") {
            return std::str::from_utf8(value[1..].trim_ascii())
                .ok()
                .filter(|host| host.is_ascii());
        }
    }
    None
}

#[cfg(test)]
mod tests;
