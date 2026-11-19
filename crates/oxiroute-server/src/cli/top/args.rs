use std::time::Duration;

use clap::Args;

use super::{
    history::Window,
    model::Sort,
    theme::{ColorChoice, ThemeName},
    views::View,
};
use crate::cli::{error::CliError, parse::parse_age_ms};

const MIN_INTERVAL_MS: u64 = 1_000;

#[derive(Args)]
pub(super) struct Printing {
    /// Print one measured snapshot as text and exit: two samples one window apart, so rates are real. This is also what happens without a terminal.
    #[arg(long)]
    pub(super) once: bool,
    /// Print one measured snapshot of the view as JSON (schema version 2) and exit.
    #[arg(long, conflicts_with = "csv")]
    pub(super) json: bool,
    /// Print one measured snapshot of the view as CSV (one line per row, then `other` and `total`) and exit.
    #[arg(long)]
    pub(super) csv: bool,
}

#[derive(Args)]
pub(super) struct Terminal {
    /// Do not capture the mouse, so the terminal keeps its own text selection.
    #[arg(long)]
    pub(super) no_mouse: bool,
    /// Draw with ASCII only: plain borders, `#` bars, `^`/`v` arrows.
    #[arg(long)]
    pub(super) ascii: bool,
    /// Forget the saved layout (hidden panels, columns, view order, density, theme, window) before starting.
    #[arg(long)]
    pub(super) reset_layout: bool,
}

#[derive(Args)]
pub(crate) struct TopArgs {
    /// Refresh interval, for example 1s or 5s; not below 1s, the rate the server rebuilds statistics at.
    #[arg(long, value_name = "INTERVAL", default_value = "1s", value_parser = parse_interval)]
    pub(super) interval: Duration,
    #[command(flatten)]
    pub(super) printing: Printing,
    #[command(flatten)]
    pub(super) terminal: Terminal,
    /// Rate window: 2s, 10s, 60s or life (counters over the uptime). Default: the saved choice, 10s at first; `--once` uses 2s.
    #[arg(long, value_enum)]
    pub(super) window: Option<Window>,
    /// The view to start in, or with `--once`, `--json` and `--csv` the only one printed. `pools` and `servers` are aliases of upstreams. Default: listeners.
    #[arg(long, value_enum)]
    pub(super) view: Option<View>,
    /// Show only matching rows: words, `!word` to exclude, `column:text` for one column, `ip:PREFIX` and `listener:NAME` for the clients and conns views (also asked of the server).
    #[arg(
        long,
        value_name = "EXPR",
        default_value = "",
        hide_default_value = true
    )]
    pub(super) filter: String,
    /// Sort the starting view by this column title or key, for example `REQ/s`.
    #[arg(long, value_name = "COLUMN")]
    pub(super) sort: Option<String>,
    /// Reverse the sort direction.
    #[arg(long)]
    pub(super) reverse: bool,
    /// How many clients to ask the server for, at most 1000.
    #[arg(long, value_name = "N", default_value = "200", value_parser = clap::value_parser!(u16).range(1..=1000))]
    pub(super) top: u16,
    /// Colors: auto follows `NO_COLOR` and the terminal, always, never.
    #[arg(long, value_enum, default_value = "auto")]
    pub(super) color: ColorChoice,
    /// Theme to use and remember: dark, light or mono.
    #[arg(long, value_enum)]
    pub(super) theme: Option<ThemeName>,
}

fn parse_interval(value: &str) -> Result<Duration, String> {
    let milliseconds = parse_age_ms(value)?;
    if milliseconds < MIN_INTERVAL_MS {
        return Err("the interval must be at least 1s".into());
    }
    Ok(Duration::from_millis(milliseconds))
}

impl TopArgs {
    pub(super) fn start_view(&self) -> View {
        self.view.unwrap_or(View::Listeners)
    }

    pub(super) fn start_sort(&self) -> Result<Sort, CliError> {
        let view = self.start_view();
        let sort = match &self.sort {
            None => view.default_sort(),
            Some(name) => Sort::for_column(view.columns(), Self::column(view, name)?),
        };
        Ok(Sort {
            descending: sort.descending ^ self.reverse,
            ..sort
        })
    }

    fn column(view: View, name: &str) -> Result<usize, CliError> {
        view.column_named(name).ok_or_else(|| {
            let titles = view
                .columns()
                .iter()
                .map(|column| column.title)
                .collect::<Vec<_>>();
            CliError::Local(format!(
                "unknown column {name} for the {} view; use one of {}",
                view.title(),
                titles.join(", ")
            ))
        })
    }

    pub(super) fn snapshot_views(&self) -> Vec<View> {
        self.view
            .map_or_else(|| vec![View::Listeners, View::Upstreams], |view| vec![view])
    }

    pub(super) fn machine_readable(&self) -> bool {
        self.printing.json || self.printing.csv
    }
}
