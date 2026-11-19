mod drawer;
mod footer;
mod meters;
mod overlay;
mod status;
mod summary;
mod table;
mod tabs;

use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    text::Line,
    widgets::{Block, Paragraph},
};

use super::{app::App, theme::Role};

pub(crate) fn frame(frame: &mut Frame, app: &App) {
    let Some(layout) = app.scene.layout else {
        return;
    };
    if layout.too_small {
        let text = format!(
            "terminal too small: need 60x12, have {}x{}",
            layout.area.width, layout.area.height
        );
        frame.render_widget(
            Paragraph::new(text).alignment(Alignment::Center),
            layout.area,
        );
        return;
    }
    status::draw(frame, app, &layout);
    meters::draw(frame, app, &layout);
    summary::draw(frame, app, &layout);
    tabs::draw(frame, app);
    table::draw(frame, app);
    drawer::draw(frame, app);
    footer::draw(frame, app, &layout);
    overlay::draw(frame, app);
}

pub(super) fn boxed<'a>(app: &App, title: Line<'a>) -> Block<'a> {
    Block::bordered()
        .border_set(app.theme.symbols.border)
        .border_style(app.theme.style(Role::Border))
        .title(title)
}

pub(super) fn width(text: &str) -> u16 {
    u16::try_from(text.chars().count()).unwrap_or(u16::MAX)
}

pub(super) fn right_of(rect: Rect, text_width: u16) -> Rect {
    let width = text_width.min(rect.width);
    Rect::new(rect.x + rect.width - width, rect.y, width, rect.height)
}
