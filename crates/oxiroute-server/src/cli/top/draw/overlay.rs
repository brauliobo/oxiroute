use ratatui::{
    Frame,
    text::{Line, Span},
    widgets::{Clear, Paragraph},
};

use super::boxed;
use crate::cli::top::{app::App, theme::Role};

pub(super) fn draw(frame: &mut Frame, app: &App) {
    let Some(overlay) = &app.scene.overlay else {
        return;
    };
    let theme = app.theme;
    frame.render_widget(Clear, overlay.rect);
    frame.render_widget(
        boxed(
            app,
            Line::styled(format!(" {} ", overlay.title), theme.style(Role::Title)),
        ),
        overlay.rect,
    );
    for item in &overlay.items {
        let mark = match item.on {
            Some(true) => "[x] ",
            Some(false) => "[ ] ",
            None => "",
        };
        let style = match (item.cursor, item.on) {
            (true, _) => theme.style(Role::Selected),
            (false, Some(false)) => theme.style(Role::Dim),
            _ => theme.style(Role::Normal),
        };
        let width = usize::from(item.rect.width);
        let text = format!("{mark}{}", item.text);
        let line = Line::from(Span::styled(format!("{text:<width$}"), style));
        frame.render_widget(Paragraph::new(line), item.rect);
    }
}
