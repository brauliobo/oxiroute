//! What a route compiles to for the fast lane: the lane serves a request only when its route is plain enough for the
//! lane to reproduce the classic path exactly, and the route says so once, when its generation is built.

use crate::http::action::{HttpActionPlan, RouteAccess};

/// How a request that matched the route is served.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LaneRoute {
    /// The classic path: Pingora's session and the proxy hooks.
    Classic,
    /// A fixed response written from a rendered head.
    Direct,
}

impl LaneRoute {
    pub(crate) fn compile(action: &HttpActionPlan, access: Option<&RouteAccess>) -> Self {
        match action {
            HttpActionPlan::Fixed(response)
                if response.rendered_head.is_some() && access.is_none() =>
            {
                Self::Direct
            }
            HttpActionPlan::Fixed(_)
            | HttpActionPlan::Proxy(_)
            | HttpActionPlan::Redirect(_)
            | HttpActionPlan::Static(_) => Self::Classic,
        }
    }
}
