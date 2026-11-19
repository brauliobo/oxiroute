use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    symbols::scrollbar,
    text::{Line, Span},
    widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
};

use super::boxed;
use crate::cli::top::{
    app::App,
    model::Tone,
    scene::{ColumnSlot, RowScene},
    theme::Role,
};

fn pad(text: &str, width: u16, numeric: bool) -> String {
    let width = usize::from(width);
    if numeric {
        format!("{text:>width$}")
    } else {
        format!("{text:<width$}")
    }
}

fn row_line(
    app: &App,
    columns: &[ColumnSlot],
    row: &RowScene,
    emphasis: Option<Style>,
) -> Line<'static> {
    let theme = app.theme;
    let marker = if row.selected {
        theme.symbols.marker
    } else {
        " "
    };
    let select = |style: Style| {
        if row.selected {
            style.patch(theme.style(Role::Selected))
        } else {
            style
        }
    };
    let mut spans = vec![Span::styled(format!("{marker} "), select(Style::new()))];
    for (index, (slot, (text, tone))) in columns.iter().zip(&row.cells).enumerate() {
        if index > 0 {
            spans.push(Span::styled(" ", select(Style::new())));
        }
        let base =
            emphasis.map_or_else(|| theme.tone(*tone), |style| theme.tone(*tone).patch(style));
        spans.push(Span::styled(
            pad(text, slot.width, slot.numeric),
            select(base),
        ));
    }
    Line::from(spans)
}

fn header_line(app: &App, columns: &[ColumnSlot]) -> Line<'static> {
    let theme = app.theme;
    let mut spans = vec![Span::raw("  ")];
    for (index, slot) in columns.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(" "));
        }
        let arrow = match slot.sorted {
            Some(true) => theme.symbols.descending,
            Some(false) => theme.symbols.ascending,
            None => "",
        };
        let text = format!("{}{arrow}", slot.title);
        spans.push(Span::styled(
            pad(&text, slot.width, slot.numeric),
            theme.style(Role::Header),
        ));
    }
    Line::from(spans)
}

pub(super) fn draw(frame: &mut Frame, app: &App) {
    let table = &app.scene.table;
    let theme = app.theme;
    let title = Line::styled(format!(" {} ", table.title), theme.style(Role::Title));
    let inner = if app.scene.layout.is_some_and(|layout| layout.bordered) {
        let block = boxed(app, title);
        let inner = block.inner(table.outer);
        frame.render_widget(block, table.outer);
        inner
    } else {
        frame.render_widget(
            Paragraph::new(title),
            Rect::new(table.outer.x, table.outer.y, table.outer.width, 1),
        );
        table.inner
    };
    if inner.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(header_line(app, &table.columns)),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    if let Some(notice) = &table.notice {
        let style = theme.tone(Tone::Warn);
        frame.render_widget(
            Paragraph::new(Line::styled(format!("  {notice}"), style)),
            table.body,
        );
    }
    let lines = table
        .rows
        .iter()
        .map(|row| row_line(app, &table.columns, row, None))
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), table.body);
    for row in &table.pinned {
        let line = row_line(app, &table.columns, row, Some(theme.style(Role::Accent)));
        frame.render_widget(
            Paragraph::new(line),
            Rect::new(inner.x, row.y, inner.width, 1),
        );
    }
    if table.total > table.viewport && table.viewport > 0 {
        let symbols = if theme.symbols.fill == "#" {
            scrollbar::Set {
                track: "|",
                thumb: "#",
                begin: "",
                end: "",
            }
        } else {
            scrollbar::VERTICAL
        };
        let mut state = ScrollbarState::new(table.total - table.viewport).position(table.offset);
        let widget = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .symbols(symbols)
            .begin_symbol(None)
            .end_symbol(None)
            .style(theme.style(Role::Border));
        frame.render_stateful_widget(widget, table.body, &mut state);
    }
}
