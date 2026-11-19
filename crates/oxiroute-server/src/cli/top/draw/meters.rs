use ratatui::{
    Frame,
    layout::{Constraint, Layout as Split, Rect},
    text::{Line, Span},
    widgets::{Paragraph, Sparkline},
};

use super::boxed;
use crate::cli::{
    top::{
        app::App,
        history::Point,
        layout::{Layout, Meters},
        theme::Role,
        totals::Totals,
    },
    units,
};

struct Readings {
    requests: String,
    input: String,
    output: String,
    errors: String,
    totals: Totals,
}

fn readings(app: &App) -> Option<Readings> {
    let sample = app.history.latest()?;
    let last = app.history.series().last().copied().unwrap_or_default();
    let totals = Totals::of(&sample.snapshot);
    let share = units::percent(totals.errors, totals.requests)
        .map_or_else(|| "-".to_owned(), |share| format!("{share:.1}%"));
    Some(Readings {
        requests: format!("{}/s", units::rate(last.requests)),
        input: app.units().rate_text(last.bytes_in),
        output: app.units().rate_text(last.bytes_out),
        errors: share,
        totals,
    })
}

pub(super) fn draw(frame: &mut Frame, app: &App, layout: &Layout) {
    let Some(meters) = layout.meters else {
        return;
    };
    let Some(readings) = readings(app) else {
        return;
    };
    match meters {
        Meters::Boxes(rect) => boxes(frame, app, rect, &readings),
        Meters::Strip(rect) => strip(
            frame,
            app,
            rect,
            &readings,
            layout.tier >= crate::cli::top::layout::Tier::Compact,
        ),
        Meters::Title(rect) => title(frame, app, rect, &readings),
    }
}

fn spark(frame: &mut Frame, app: &App, rect: Rect, pick: fn(&Point) -> f64) {
    let bars = app.history.series().bars(pick);
    let skip = bars.len().saturating_sub(usize::from(rect.width));
    let widget = Sparkline::default()
        .data(bars.into_iter().skip(skip))
        .bar_set(app.theme.symbols.bars.clone())
        .style(app.theme.style(Role::Title));
    frame.render_widget(widget, rect);
}

fn meter_bar(app: &App, fraction: f64, width: u16) -> Vec<Span<'static>> {
    let symbols = app.theme.symbols;
    let filled = usize::from(width).min(
        units::saturating_u64(fraction.clamp(0.0, 1.0) * f64::from(width))
            .try_into()
            .unwrap_or(0),
    );
    vec![
        Span::styled(symbols.fill.repeat(filled), app.theme.meter(fraction)),
        Span::styled(
            symbols.vacant.repeat(usize::from(width) - filled),
            app.theme.style(Role::Dim),
        ),
    ]
}

fn boxes(frame: &mut Frame, app: &App, rect: Rect, readings: &Readings) {
    let cells = Split::horizontal([Constraint::Ratio(1, 4); 4]).split(rect);
    let series = app.history.series();
    let dim = app.theme.style(Role::Dim);
    let mut traffic =
        |index: usize, title: &str, now: &str, pick: fn(&Point) -> f64, total: u64| {
            let block = boxed(
                app,
                Line::styled(format!(" {title} "), app.theme.style(Role::Title)),
            );
            let inner = block.inner(cells[index]);
            frame.render_widget(block, cells[index]);
            let lines = Split::vertical([Constraint::Length(1); 3]).split(inner);
            frame.render_widget(
                Paragraph::new(Line::styled(now.to_owned(), app.theme.style(Role::Accent))),
                lines[0],
            );
            spark(frame, app, lines[1], pick);
            let peak = series.peak(pick);
            let bottom = if index == 0 {
                format!("peak {}/s  tot {}", units::rate(peak), units::count(total))
            } else {
                format!(
                    "peak {}  tot {}",
                    app.units().rate_text(peak),
                    app.units().traffic(units::as_f64(total), false)
                )
            };
            frame.render_widget(Paragraph::new(Line::styled(bottom, dim)), lines[2]);
        };
    traffic(
        0,
        "Requests/s",
        &format!("{}  err {}", readings.requests, readings.errors),
        |point| point.requests,
        readings.totals.requests,
    );
    traffic(
        1,
        "In",
        &readings.input,
        |point| point.bytes_in,
        readings.totals.bytes_in,
    );
    traffic(
        2,
        "Out",
        &readings.output,
        |point| point.bytes_out,
        readings.totals.bytes_out,
    );
    load(frame, app, cells[3]);
}

fn load(frame: &mut Frame, app: &App, rect: Rect) {
    let Some(sample) = app.history.latest() else {
        return;
    };
    let process = &sample.snapshot.process;
    let block = boxed(app, Line::styled(" Load ", app.theme.style(Role::Title)));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let lines = Split::vertical([Constraint::Length(1); 3]).split(inner);
    let bar_width = inner.width.saturating_sub(16).clamp(4, 12);
    let cpu = process.cpu_percent.unwrap_or(0.0);
    let mut cpu_line = vec![Span::raw("cpu  ")];
    cpu_line.extend(meter_bar(app, cpu / 100.0, bar_width));
    cpu_line.push(Span::raw(format!(" {cpu:.0}%")));
    let used = process.max_connections.map_or(0.0, |max| {
        units::as_f64(process.active_connections) / units::as_f64(max.max(1))
    });
    let mut conns = vec![Span::raw("conn ")];
    conns.extend(meter_bar(app, used, bar_width));
    conns.push(Span::raw(format!(" {}", process.active_connections)));
    let memory = crate::cli::top::headline::optional(process.resident_memory_bytes, units::bytes);
    let fds = crate::cli::top::headline::optional(process.open_file_descriptors, |count| {
        count.to_string()
    });
    frame.render_widget(Paragraph::new(Line::from(cpu_line)), lines[0]);
    frame.render_widget(Paragraph::new(Line::from(conns)), lines[1]);
    frame.render_widget(
        Paragraph::new(Line::styled(
            format!("mem {memory}  fds {fds}"),
            app.theme.style(Role::Dim),
        )),
        lines[2],
    );
}

fn strip(frame: &mut Frame, app: &App, rect: Rect, readings: &Readings, sparks: bool) {
    let process = app.history.latest().map(|sample| &sample.snapshot.process);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut place = |label: &str, value: &str, pick: fn(&Point) -> f64| {
        spans.push(Span::styled(
            format!("{label} "),
            app.theme.style(Role::Dim),
        ));
        spans.push(Span::styled(
            value.to_owned(),
            app.theme.style(Role::Accent),
        ));
        if sparks {
            let bars = app.history.series().bars(pick);
            let skip = bars.len().saturating_sub(8);
            let ramp = bars.into_iter().skip(skip).collect::<Vec<_>>();
            let peak = ramp.iter().copied().max().unwrap_or(0).max(1);
            let glyphs = ramp
                .iter()
                .map(|value| level(app, *value, peak))
                .collect::<String>();
            spans.push(Span::styled(
                format!(" {glyphs}"),
                app.theme.style(Role::Title),
            ));
        }
        spans.push(Span::raw("  "));
    };
    place("req/s", &readings.requests, |point| point.requests);
    place("in", &readings.input, |point| point.bytes_in);
    place("out", &readings.output, |point| point.bytes_out);
    if let Some(process) = process {
        spans.push(Span::raw(format!(
            "conn {}  cpu {}",
            process.active_connections,
            crate::cli::top::headline::optional(process.cpu_percent, |cpu| format!("{cpu:.0}%"))
        )));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
}

fn level(app: &App, value: u64, peak: u64) -> String {
    let bars = app.theme.symbols.bars;
    let steps = [
        bars.empty,
        bars.one_eighth,
        bars.one_quarter,
        bars.three_eighths,
        bars.half,
        bars.five_eighths,
        bars.three_quarters,
        bars.seven_eighths,
        bars.full,
    ];
    let index = usize::try_from(value * 8 / peak).unwrap_or(8).min(8);
    steps[index].to_owned()
}

fn title(frame: &mut Frame, app: &App, rect: Rect, readings: &Readings) {
    let symbols = app.theme.symbols;
    let text = format!(
        "{} Meters  req {}  in {}  out {}  err {}  (h expands, H hides)",
        symbols.closed, readings.requests, readings.input, readings.output, readings.errors
    );
    frame.render_widget(
        Paragraph::new(Line::styled(text, app.theme.style(Role::Dim))),
        rect,
    );
}
