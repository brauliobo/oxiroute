use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::cli::top::{
    app::{App, Layer},
    layout::Layout,
    theme::Role,
};

pub(super) fn draw(frame: &mut Frame, app: &App, layout: &Layout) {
    let theme = app.theme;
    let message = match (app.layers.last(), &app.error, &app.message) {
        (Some(Layer::Filter(draft)), ..) => Line::from(vec![
            Span::styled("filter ", theme.style(Role::Accent)),
            Span::raw(draft.clone()),
            Span::styled("_", theme.style(Role::Accent)),
            Span::styled(
                "   words, !not, column:text, ip:PREFIX",
                theme.style(Role::Dim),
            ),
        ]),
        (_, Some(error), _) => Line::styled(format!("error: {error}"), theme.style(Role::Bad)),
        (_, _, Some(message)) => Line::styled(message.clone(), theme.style(Role::Warn)),
        _ => Line::default(),
    };
    frame.render_widget(Paragraph::new(message), layout.message);
    let mut first_restore = None;
    for chip in &app.scene.chips {
        let line = Line::from(vec![
            Span::styled(format!("{} ", chip.key), theme.style(Role::Dim)),
            Span::styled(format!("{} ", chip.name), theme.style(Role::Chip)),
        ]);
        frame.render_widget(
            Paragraph::new(line),
            Rect::new(chip.rect.x, chip.rect.y, chip.rect.width + 1, 1),
        );
        if chip.restore && first_restore.is_none() {
            first_restore = Some(chip.rect.x);
        }
    }
    if let Some(x) = first_restore.filter(|x| *x >= layout.keys.x + 8) {
        frame.render_widget(
            Paragraph::new(Line::styled("hidden: ", theme.style(Role::Dim))),
            Rect::new(x - 8, layout.keys.y, 8, 1),
        );
    }
}
