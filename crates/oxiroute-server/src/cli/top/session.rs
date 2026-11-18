use std::{
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread,
    time::Duration,
};

use ratatui::crossterm::event::{self, Event, KeyEventKind};

use super::{
    app::{App, Control, Effect},
    draw,
    keys::Key,
    source::{self, Sample},
};
use crate::cli::{error::CliError, net::Client};

enum Message {
    Key(Key),
    Redraw,
    Sample(Result<Box<Sample>, String>),
}

struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

pub(super) fn run(mut app: App, client: Client, first: Sample) -> Result<(), CliError> {
    app.sample(Ok(first));
    let mut terminal = ratatui::try_init().map_err(|error| CliError::Local(error.to_string()))?;
    let _restore = Restore;
    let (messages, inbox) = mpsc::channel();
    let (control, controls) = mpsc::channel();
    let interval = app.interval;
    let initial = app.control();
    let sampler = messages.clone();
    thread::spawn(move || sample_loop(&client, interval, initial, &controls, &sampler));
    thread::spawn(move || read_terminal(&messages));
    loop {
        let table = app.table();
        app.clamp(table.as_ref().map_or(0, |table| table.rows.len()));
        terminal
            .draw(|frame| draw::frame(frame, &app, table.as_ref()))
            .map_err(|error| CliError::Local(error.to_string()))?;
        match inbox.recv() {
            Ok(Message::Key(key)) => match app.key(key) {
                Effect::Quit => return Ok(()),
                Effect::Retarget => {
                    if control.send(app.control()).is_err() {
                        return Ok(());
                    }
                }
                Effect::Idle => {}
            },
            Ok(Message::Sample(sample)) => app.sample(sample.map(|sample| *sample)),
            Ok(Message::Redraw) => {}
            Err(_) => return Ok(()),
        }
    }
}

fn sample_loop(
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
            let sample = source::sample(client, control.requests)
                .map(Box::new)
                .map_err(|error| error.to_string());
            if messages.send(Message::Sample(sample)).is_err() {
                return;
            }
        }
    }
}

fn read_terminal(messages: &Sender<Message>) {
    while let Ok(event) = event::read() {
        let message = match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                Key::from_event(key).map(Message::Key)
            }
            Event::Resize(..) => Some(Message::Redraw),
            _ => None,
        };
        if message.is_some_and(|message| messages.send(message).is_err()) {
            return;
        }
    }
}
