use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Cell as TableCell, Paragraph, Row as TableRow, Table as TableWidget, TableState},
};

use super::{
    app::{App, Mode},
    rates::Sampling,
    rows::{Table, Tone, View},
    summary,
};
use crate::cli::units;

const MAX_WIDTH: usize = 40;
const HELP: &str =
    "q quit  Tab 1-4 view  < > sort column  r reverse  / filter  p pause  j k select";

pub(super) fn frame(frame: &mut Frame, app: &App, table: Option<&Table>) {
    let [summary, tabs, body, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .areas(frame.area());
    frame.render_widget(Paragraph::new(summary_lines(app)), summary);
    frame.render_widget(Paragraph::new(tab_line(app)), tabs);
    if let Some(table) = table {
        table_widget(frame, app, table, body);
    }
    frame.render_widget(Paragraph::new(footer_lines(app)), footer);
}

fn summary_lines(app: &App) -> Text<'static> {
    let Some(current) = &app.current else {
        return Text::from("waiting for the first snapshot");
    };
    let sampling = Sampling::new(&current.snapshot, app.previous.as_ref());
    summary::lines(&sampling)
        .into_iter()
        .map(Line::from)
        .collect()
}

fn tab_line(app: &App) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, view) in View::ALL.into_iter().enumerate() {
        let style = if view == app.view {
            Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::new()
        };
        spans.push(Span::styled(
            format!(" {}:{} ", index + 1, view.title()),
            style,
        ));
    }
    let sort = app.sort();
    let column = app.view.columns()[sort.column].title;
    let direction = if sort.descending { "desc" } else { "asc" };
    let basis = if app.previous.is_some() {
        "window"
    } else {
        "lifetime"
    };
    spans.push(Span::raw(format!(
        "  sort {column} {direction}  rates/latency {basis}"
    )));
    if !app.filter.is_empty() {
        spans.push(Span::raw(format!("  filter \"{}\"", app.filter)));
    }
    if let Some(requests) = app
        .current
        .as_ref()
        .and_then(|current| current.requests.as_ref())
    {
        let shown = requests.items.len();
        let truncated = if requests.truncated {
            format!(", first {shown} shown")
        } else {
            String::new()
        };
        spans.push(Span::raw(format!(
            "  {} in flight{truncated}",
            requests.total.get()
        )));
    }
    spans.push(Span::raw(format!(
        "  every {}",
        units::duration(duration_ms(app))
    )));
    if app.paused {
        spans.push(Span::styled(
            "  PAUSED",
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
}

fn duration_ms(app: &App) -> u64 {
    u64::try_from(app.interval.as_millis()).unwrap_or(u64::MAX)
}

fn footer_lines(app: &App) -> Text<'static> {
    let status = app.error.as_ref().map_or_else(Line::default, |error| {
        Line::styled(format!("error: {error}"), Style::new().fg(Color::Red))
    });
    let controls = match &app.mode {
        Mode::Filter(draft) => Line::from(format!("filter: {draft}_  (Enter keep, Esc clear)")),
        Mode::Browse => Line::styled(HELP, Style::new().fg(Color::DarkGray)),
    };
    Text::from(vec![status, controls])
}

fn tone_style(tone: Tone) -> Style {
    match tone {
        Tone::Plain => Style::new(),
        Tone::Good => Style::new().fg(Color::Green),
        Tone::Warn => Style::new().fg(Color::Yellow),
        Tone::Bad => Style::new().fg(Color::Red),
        Tone::Dim => Style::new().fg(Color::DarkGray),
    }
}

fn table_widget(frame: &mut Frame, app: &App, table: &Table, area: Rect) {
    let columns = table.view.columns();
    let sort = app.sort();
    let marker = if sort.descending { "v" } else { "^" };
    let aligned = |text: String, numeric: bool| {
        let alignment = if numeric {
            Alignment::Right
        } else {
            Alignment::Left
        };
        Text::from(text).alignment(alignment)
    };
    let header = TableRow::new(columns.iter().enumerate().map(|(index, column)| {
        let title = if index == sort.column {
            format!("{}{marker}", column.title)
        } else {
            column.title.to_owned()
        };
        TableCell::new(aligned(title, column.numeric))
    }))
    .style(Style::new().add_modifier(Modifier::BOLD | Modifier::UNDERLINED));
    let body = table.rows.iter().map(|row| {
        TableRow::new(row.cells.iter().zip(&columns).map(|(cell, column)| {
            TableCell::new(aligned(cell.text.clone(), column.numeric)).style(tone_style(cell.tone))
        }))
    });
    let widths = table
        .widths()
        .into_iter()
        .enumerate()
        .map(|(index, width)| {
            let width = width.min(MAX_WIDTH) + usize::from(index == sort.column);
            Constraint::Length(u16::try_from(width).unwrap_or(u16::MAX))
        });
    let widget = TableWidget::new(body, widths)
        .header(header)
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut state =
        TableState::default().with_selected((!table.rows.is_empty()).then_some(app.selected));
    frame.render_stateful_widget(widget, area, &mut state);
}
