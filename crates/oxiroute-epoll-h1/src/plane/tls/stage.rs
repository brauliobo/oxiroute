use std::{os::fd::OwnedFd, sync::Arc};

use super::{Progress, Session};
use crate::plane::{
    client::{Client, Stage},
    link::Link,
    reactor::{Reactor, Step},
    service::Service,
};

impl<S: Service> Reactor<S> {
    pub(crate) fn open_link(&self, listener: usize, fd: OwnedFd) -> (Link, Stage) {
        match &self.tls[listener] {
            Some(router) => (
                Link::secure(fd, Session::new(Arc::clone(router))),
                Stage::Handshake,
            ),
            None => (Link::new(fd), Stage::Idle),
        }
    }

    pub(crate) fn handshake(client: &mut Client<S>) -> Step {
        match client.link.handshake() {
            Progress::Blocked => Step::Wait,
            Progress::Failed => Step::Close,
            Progress::Established { h1: true } => {
                client.stage = Stage::Idle;
                Step::Again
            }
            Progress::Established { h1: false } => Step::Handoff,
        }
    }
}
