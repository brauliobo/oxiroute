use ratatui::{
    Frame,
    style::Modifier,
    text::{Line, Span},
    widgets::Paragraph,
};

use super::right_of;
use crate::cli::top::{app::App, theme::Role};

pub(super) fn draw(frame: &mut Frame, app: &App) {
    let tabs = &app.scene.tabs;
    let theme = app.theme;
    for slot in &tabs.slots {
        let base = if slot.active {
            theme
                .style(Role::Accent)
                .add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            theme.style(Role::Normal)
        };
        let mut spans = vec![Span::styled(
            format!(" {} {}", slot.number, slot.view.title()),
            base,
        )];
        if let Some((text, tone)) = &slot.badge {
            let style = if slot.active { base } else { theme.tone(*tone) };
            spans.push(Span::styled(format!(" {text}"), style));
        }
        spans.push(Span::styled(" ", base));
        frame.render_widget(Paragraph::new(Line::from(spans)), slot.rect);
    }
    let status = Line::styled(tabs.status.clone(), theme.style(Role::Dim));
    frame.render_widget(
        Paragraph::new(status),
        right_of(tabs.status_rect, tabs.status_rect.width),
    );
}
