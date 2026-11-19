use ratatui::{
    Frame,
    text::{Line, Span},
    widgets::Paragraph,
};

use super::boxed;
use crate::cli::top::{
    app::App,
    layout::{Layout, Summary},
    model::Tone,
    theme::Role,
    views::Chip,
};

fn spans(app: &App, chips: &[Chip], limit: usize) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for chip in chips.iter().take(limit) {
        spans.push(Span::styled(
            format!("{} ", chip.label),
            app.theme.style(Role::Dim),
        ));
        spans.push(Span::styled(chip.value.clone(), app.theme.tone(chip.tone)));
        spans.push(Span::raw("   "));
    }
    spans
}

pub(super) fn draw(frame: &mut Frame, app: &App, layout: &Layout) {
    let Some(summary) = layout.summary else {
        return;
    };
    let mut chips = app
        .sampling()
        .map(|sampling| app.view.summary(&sampling))
        .unwrap_or_default();
    if chips.is_empty() {
        chips.push(Chip::new("no data", "for this view yet").toned(Tone::Dim));
    }
    match summary {
        Summary::Box(rect) => {
            let title = Line::styled(
                format!(" {} summary ", app.view.title()),
                app.theme.style(Role::Title),
            );
            let block = boxed(app, title);
            let inner = block.inner(rect);
            frame.render_widget(block, rect);
            frame.render_widget(
                Paragraph::new(Line::from(spans(app, &chips, usize::MAX))),
                inner,
            );
        }
        Summary::Title(rect) => {
            let mut line = vec![Span::styled(
                format!(
                    "{} {} summary  ",
                    app.theme.symbols.closed,
                    app.view.title()
                ),
                app.theme.style(Role::Dim),
            )];
            line.extend(spans(app, &chips, 3));
            frame.render_widget(Paragraph::new(Line::from(line)), rect);
        }
    }
}
