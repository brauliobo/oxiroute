use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect};

use super::fixture::{full, load, sample, snapshot};
use crate::cli::top::{
    app::{App, Settings},
    draw,
    history::Window,
    prefs::Prefs,
    source::Sample,
    theme::{Capabilities, Depth},
    views::View,
};

pub(super) const PLAIN: Capabilities = Capabilities {
    depth: Depth::None,
    unicode: true,
};

pub(super) fn app(view: View, prefs: Prefs, capabilities: Capabilities) -> App {
    App::new(Settings {
        view,
        sort: None,
        filter: String::new(),
        limit: 200,
        mouse: false,
        prefs,
        window: Window::Short,
        capabilities,
        notice: None,
    })
}

pub(super) fn first_sample() -> Sample {
    sample(snapshot(10_000, 100_000, &load(1_000, 0, 0)))
}

pub(super) fn second_sample() -> Sample {
    sample(snapshot(12_000, 102_000, &load(3_000, 40, 60)))
}

pub(super) fn running(view: View) -> App {
    let mut app = app(view, Prefs::default(), PLAIN);
    app.sample(Ok(first_sample()));
    app.sample(Ok(second_sample()));
    app
}

pub(super) fn running_full(view: View) -> App {
    let mut app = app(view, Prefs::default(), PLAIN);
    app.sample(Ok(full(snapshot(10_000, 100_000, &load(1_000, 0, 0)), 1)));
    app.sample(Ok(full(snapshot(12_000, 102_000, &load(3_000, 40, 60)), 3)));
    app
}

pub(super) fn draw(app: &mut App, width: u16, height: u16) -> Buffer {
    app.prepare(Rect::new(0, 0, width, height));
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| draw::frame(frame, app))
        .expect("draw");
    terminal.backend().buffer().clone()
}

pub(super) fn text(buffer: &Buffer) -> String {
    let width = usize::from(buffer.area.width);
    buffer
        .content()
        .chunks(width)
        .map(|row| {
            row.iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn screen(app: &mut App, width: u16, height: u16) -> String {
    text(&draw(app, width, height))
}

pub(super) fn line_with<'a>(screen: &'a str, needle: &str) -> &'a str {
    screen
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no line holds {needle}:\n{screen}"))
}
