use std::sync::{Arc, Mutex, PoisonError};

use http::{Method, uri::Authority};

use crate::{
    http::{
        action::HttpRoutePlan,
        proxy::lane::{LaneFront, head::Target},
    },
    sharded::Sharded,
};

type Found = (Arc<HttpRoutePlan>, Authority);

/// The route the latest request of a thread went to, with what it was selected for. The routes of a generation do not
/// change, so a request that asks for the same method, host and path is routed as that one was.
#[derive(Default)]
struct Latest {
    method: Option<Method>,
    host: String,
    path: String,
    found: Option<Found>,
}

impl Latest {
    fn answers(&self, method: &Method, target: &Target<'_>) -> Option<Found> {
        (self.method.as_ref() == Some(method)
            && self.host == target.host
            && self.path == target.path)
            .then(|| self.found.clone())
            .flatten()
    }

    fn keep(&mut self, method: &Method, target: &Target<'_>, found: Found) {
        self.method = Some(method.clone());
        self.host.clear();
        self.host.push_str(target.host);
        self.path.clear();
        self.path.push_str(target.path);
        self.found = Some(found);
    }
}

/// Routes the requests of the engine threads of one generation: each thread remembers its latest.
pub(super) struct Routes(Sharded<Mutex<Latest>>);

impl Routes {
    pub(super) fn new() -> Self {
        Self(Sharded::new(Mutex::default))
    }

    pub(super) fn resolve(
        &self,
        front: &LaneFront,
        method: &Method,
        target: &Target<'_>,
    ) -> Option<Found> {
        let mut latest = self
            .0
            .local()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(found) = latest.answers(method, target)
            && !front.holds_challenge(target.path)
        {
            return Some(found);
        }
        let (route, authority, _) = front.resolve(method, target)?;
        latest.keep(method, target, (Arc::clone(&route), authority.clone()));
        Some((route, authority))
    }
}
