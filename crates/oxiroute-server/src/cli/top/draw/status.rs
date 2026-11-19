use ratatui::{
    Frame,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::Paragraph,
};

use super::{right_of, width};
use crate::cli::top::{app::App, headline, layout::Layout, theme::Role};

pub(super) fn draw(frame: &mut Frame, app: &App, layout: &Layout) {
    let Some(rect) = layout.status else {
        return;
    };
    let theme = app.theme;
    let symbols = theme.symbols;
    let left = app.history.latest().map_or_else(
        || "waiting for the first snapshot".to_owned(),
        |sample| headline::identity(&sample.snapshot),
    );
    let (badge, role) = match (app.switches.paused, app.error.is_some()) {
        (true, _) => (" PAUSED ".to_owned(), Role::Warn),
        (false, true) => (" STALE ".to_owned(), Role::Bad),
        _ => (format!(" {} LIVE ", symbols.live), Role::Good),
    };
    let window = format!(" {} rates ", app.window.label());
    let total = width(&window) + width(&badge);
    let left_rect = Rect::new(rect.x, rect.y, rect.width.saturating_sub(total + 1), 1);
    frame.render_widget(
        Paragraph::new(Line::styled(format!(" {left}"), theme.style(Role::Title))),
        left_rect,
    );
    let line = Line::from(vec![
        Span::styled(window, theme.style(Role::Dim)),
        Span::styled(
            badge,
            theme
                .style(role)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), right_of(rect, total));
}
