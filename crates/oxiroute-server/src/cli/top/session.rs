mod sampler;
mod terminal;

use std::{io::stdout, sync::mpsc, thread, time::Duration};

use ratatui::{
    crossterm::{
        event::{DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind},
        execute,
    },
    layout::Rect,
};

use super::{
    app::{App, Effect, Key},
    draw,
    prefs::Store,
    source::Sample,
};
use crate::cli::{error::CliError, net::Client};

enum Message {
    Input(Event),
    Sample(Result<Box<Sample>, String>),
}

struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        let _ = execute!(stdout(), DisableMouseCapture);
        ratatui::restore();
    }
}

fn local(error: impl std::fmt::Display) -> CliError {
    CliError::Local(error.to_string())
}

fn capture(on: bool) -> Result<(), CliError> {
    if on {
        execute!(stdout(), EnableMouseCapture).map_err(local)
    } else {
        execute!(stdout(), DisableMouseCapture).map_err(local)
    }
}

fn persist(app: &mut App, store: &Store) {
    if std::mem::take(&mut app.prefs_dirty)
        && let Err(error) = store.save(&app.prefs)
    {
        app.message = Some(error.to_string());
    }
}

fn handle(app: &mut App, message: Message) -> Result<bool, CliError> {
    let effect = match message {
        Message::Sample(sample) => {
            app.sample(sample.map(|sample| *sample));
            Effect::Idle
        }
        Message::Input(Event::Key(key)) if key.kind == KeyEventKind::Press => {
            Key::from_event(key).map_or(Effect::Idle, |key| app.key(key))
        }
        Message::Input(Event::Mouse(mouse)) => app.mouse(mouse),
        Message::Input(_) => Effect::Idle,
    };
    match effect {
        Effect::Quit => return Ok(true),
        Effect::Mouse(on) => capture(on)?,
        Effect::Idle => {}
    }
    Ok(false)
}

pub(crate) fn run(
    mut app: App,
    client: Client,
    first: Sample,
    interval: Duration,
    store: &Store,
) -> Result<(), CliError> {
    app.sample(Ok(first));
    let mut terminal = ratatui::try_init().map_err(local)?;
    let _restore = Restore;
    if app.switches.mouse {
        capture(true)?;
    }
    let (messages, inbox) = mpsc::channel();
    let (control, controls) = mpsc::channel();
    let mut sent = app.control();
    let initial = sent.clone();
    let sampler = messages.clone();
    thread::spawn(move || sampler::run(&client, interval, initial, &controls, &sampler));
    thread::spawn(move || terminal::read(&messages));
    loop {
        let size = terminal.size().map_err(local)?;
        app.prepare(Rect::new(0, 0, size.width, size.height));
        terminal
            .draw(|frame| draw::frame(frame, &app))
            .map_err(local)?;
        persist(&mut app, store);
        let Ok(message) = inbox.recv() else {
            return Ok(());
        };
        let mut quit = handle(&mut app, message)?;
        while !quit {
            let Ok(message) = inbox.try_recv() else {
                break;
            };
            quit = handle(&mut app, message)?;
        }
        persist(&mut app, store);
        if quit {
            return Ok(());
        }
        let wanted = app.control();
        if wanted != sent {
            sent = wanted.clone();
            if control.send(wanted).is_err() {
                return Ok(());
            }
        }
    }
}
