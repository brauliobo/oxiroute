use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{Paragraph, Sparkline},
};

use super::boxed;
use crate::cli::top::{app::App, scene::DrawerScene, theme::Role};

pub(super) fn draw(frame: &mut Frame, app: &App) {
    let Some(drawer) = &app.scene.drawer else {
        return;
    };
    let theme = app.theme;
    if !drawer.open {
        let hint = if drawer.title.is_empty() {
            String::new()
        } else {
            format!(" {}", drawer.title)
        };
        let text = format!(
            "{} Detail{hint}  (Enter or click opens)",
            theme.symbols.closed
        );
        frame.render_widget(
            Paragraph::new(Line::styled(text, theme.style(Role::Dim))),
            drawer.rect,
        );
        return;
    }
    let block = boxed(
        app,
        Line::styled(
            format!(" Detail {} ", drawer.title),
            theme.style(Role::Title),
        ),
    );
    let inner = block.inner(drawer.rect);
    frame.render_widget(block, drawer.rect);
    let mut y = inner.y;
    let bottom = inner.y + inner.height;
    if drawer.gone {
        frame.render_widget(
            Paragraph::new(Line::styled(
                "gone: this row is no longer in the data",
                theme.style(Role::Warn),
            )),
            Rect::new(inner.x, y, inner.width, 1),
        );
        y += 1;
    }
    y = sparks(frame, app, drawer, inner, y, bottom);
    let width = label_width(drawer.lines.iter().map(|(label, _)| *label));
    for (label, value) in &drawer.lines {
        if y >= bottom {
            break;
        }
        let line = Line::from(vec![
            Span::styled(format!("{label:<width$}"), theme.style(Role::Dim)),
            Span::raw(value.clone()),
        ]);
        frame.render_widget(Paragraph::new(line), Rect::new(inner.x, y, inner.width, 1));
        y += 1;
    }
}

fn label_width<'a>(labels: impl Iterator<Item = &'a str>) -> usize {
    labels.map(|label| label.chars().count()).max().unwrap_or(0) + 1
}

fn sparks(
    frame: &mut Frame,
    app: &App,
    drawer: &DrawerScene,
    inner: Rect,
    mut y: u16,
    bottom: u16,
) -> u16 {
    let theme = app.theme;
    let width = label_width(drawer.sparks.iter().map(|spark| spark.label.as_str())).max(8);
    for spark in &drawer.sparks {
        if y >= bottom {
            break;
        }
        let label = format!("{:<width$}{:>7} ", spark.label, spark.value);
        let used = u16::try_from(label.chars().count())
            .unwrap_or(0)
            .min(inner.width);
        frame.render_widget(
            Paragraph::new(Line::styled(label, theme.style(Role::Dim))),
            Rect::new(inner.x, y, used, 1),
        );
        let area = Rect::new(inner.x + used, y, inner.width - used, 1);
        let skip = spark.bars.len().saturating_sub(usize::from(area.width));
        let widget = Sparkline::default()
            .data(spark.bars.iter().copied().skip(skip))
            .bar_set(theme.symbols.bars.clone())
            .style(theme.style(Role::Title));
        frame.render_widget(widget, area);
        y += 1;
    }
    y
}
