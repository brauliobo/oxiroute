//! The route of a request, decided from the bytes of its first line (and its `Host` line when the routes depend on the
//! host) before the head is scanned. A request whose route the lane does not serve goes to the classic path at once:
//! scanning it first would parse a head that the session parses again.
//!
//! The decision only ever shortcuts to the classic path, which is the reference, so a wrong guess about a head the
//! scan would refuse costs nothing. A request that may be the lane's keeps its route, so the scan does not select it a
//! second time.

use std::sync::Arc;

use http::{Method, uri::Authority};

use super::{LaneRoute, head};
use crate::{HttpServicePlan, http::action::HttpRoutePlan};

/// The first line of a request head and, when the routes select on the host, its first `Host` field.
pub(super) struct Peek<'a> {
    method: Method,
    path: &'a str,
    host: Option<&'a str>,
}

/// What the first bytes of a request say about how it is served.
pub(super) enum Early<'a> {
    /// The bytes do not tell (a partial or odd head): the scan decides.
    Unknown,
    /// No route of the lane serves it.
    Classic,
    /// The route that serves it if its head scans clean, and what the route was selected for.
    Lane(Arc<HttpRoutePlan>, Peek<'a>),
}

pub(super) fn decide<'a>(service: &HttpServicePlan, buffer: &'a [u8]) -> Early<'a> {
    let by_host = service.selects_on_host();
    let Some(peek) = Peek::of(buffer, by_host) else {
        return Early::Unknown;
    };
    let authority = if by_host {
        match peek.host.and_then(|host| host.parse::<Authority>().ok()) {
            Some(authority) => Some(authority),
            None => return Early::Unknown,
        }
    } else {
        None
    };
    match service.select_route_path(authority.as_ref(), peek.path, &peek.method) {
        Some(route) if route.lane != LaneRoute::Classic => Early::Lane(route, peek),
        _ => Early::Classic,
    }
}

impl<'a> Peek<'a> {
    fn of(buffer: &'a [u8], with_host: bool) -> Option<Self> {
        let mut lines = buffer.split(|byte| *byte == b'\n');
        let mut parts = lines
            .next()?
            .strip_suffix(b"\r")?
            .splitn(3, |byte| *byte == b' ');
        let (method, target, _version) = (parts.next()?, parts.next()?, parts.next()?);
        if !target.starts_with(b"/") {
            return None;
        }
        let path = target.split(|byte| *byte == b'?').next()?;
        let host = if with_host { host_of(lines) } else { None };
        Some(Self {
            method: Method::from_bytes(method).ok()?,
            path: std::str::from_utf8(path)
                .ok()
                .filter(|path| path.is_ascii())?,
            host,
        })
    }

    /// Whether the scanned head is the one the route was selected for.
    pub(super) fn is_of(&self, buffer: &[u8], request: &head::Request, with_host: bool) -> bool {
        self.method.as_str().as_bytes() == request.method(buffer)
            && self.path == request.path(buffer)
            && (!with_host || self.host == request.host(buffer))
    }
}

fn host_of<'a>(lines: impl Iterator<Item = &'a [u8]>) -> Option<&'a str> {
    for line in lines {
        let line = line.strip_suffix(b"\r")?;
        if line.is_empty() {
            return None;
        }
        let (name, value) = line.split_at(line.iter().position(|byte| *byte == b':')?);
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
