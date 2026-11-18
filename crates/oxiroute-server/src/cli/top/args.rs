use std::time::Duration;

use clap::Args;

use super::rows::{Sort, View};
use crate::cli::{error::CliError, parse::parse_age_ms};

const MIN_INTERVAL_MS: u64 = 1_000;

#[derive(Args)]
pub(crate) struct TopArgs {
    /// Refresh interval, for example 1s or 5s; not below 1s, the rate the server rebuilds statistics at.
    #[arg(long, value_name = "INTERVAL", default_value = "1s", value_parser = parse_interval)]
    pub(super) interval: Duration,
    /// Print one snapshot as text and exit. This is also what happens without a terminal.
    #[arg(long)]
    pub(super) once: bool,
    /// Print one snapshot as the management API's JSON document and exit.
    #[arg(long)]
    pub(super) json: bool,
    /// The view to start in, or with `--once` the only one printed. Default: listeners.
    #[arg(long, value_enum)]
    pub(super) view: Option<View>,
    /// Show only rows whose names contain this text, ignoring case.
    #[arg(
        long,
        value_name = "TEXT",
        default_value = "",
        hide_default_value = true
    )]
    pub(super) filter: String,
    /// Sort the starting view by this column title, for example `REQ/s`.
    #[arg(long, value_name = "COLUMN")]
    pub(super) sort: Option<String>,
    /// Reverse the sort direction.
    #[arg(long)]
    pub(super) reverse: bool,
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
            Some(name) => Sort::for_column(view, Self::column(view, name)?),
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
            let reason = format!(
                "unknown column {name} for the {} view; use one of {}",
                view.title(),
                titles.join(", ")
            );
            CliError::Local(reason)
        })
    }

    pub(super) fn snapshot_views(&self) -> Vec<View> {
        self.view.map_or_else(
            || vec![View::Listeners, View::Pools, View::Servers],
            |view| vec![view],
        )
    }
}
