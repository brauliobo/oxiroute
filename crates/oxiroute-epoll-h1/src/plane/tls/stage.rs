use std::{io, os::fd::OwnedFd, sync::Arc};

use super::{Progress, Session};
use crate::plane::{
    client::{Client, Stage},
    link::Link,
    reactor::{Reactor, Step},
    service::Service,
    slab::Token,
};

impl<S: Service> Reactor<S> {
    pub(crate) fn open_link(
        &self,
        listener: usize,
        fd: OwnedFd,
        token: Token,
    ) -> io::Result<(Link, Stage)> {
        let session = self.tls[listener]
            .as_ref()
            .map(|router| Session::new(Arc::clone(router)));
        let stage = if session.is_some() {
            Stage::Handshake
        } else {
            Stage::Idle
        };
        let link = self.driver.open(fd, token, session)?;
        Ok((link, stage))
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
