use std::{
    sync::mpsc::{Receiver, RecvTimeoutError, Sender},
    time::Duration,
};

use super::Message;
use crate::cli::{
    net::Client,
    top::{app::Control, source},
};

pub(super) fn run(
    client: &Client,
    interval: Duration,
    mut control: Control,
    controls: &Receiver<Control>,
    messages: &Sender<Message>,
) {
    loop {
        let next = if control.paused {
            controls.recv().map_err(|_| RecvTimeoutError::Disconnected)
        } else {
            controls.recv_timeout(interval)
        };
        match next {
            Ok(next) => control = next,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if !control.paused {
            let sample = source::sample(client, &control.wants)
                .map(Box::new)
                .map_err(|error| error.to_string());
            if messages.send(Message::Sample(sample)).is_err() {
                return;
            }
        }
    }
}
