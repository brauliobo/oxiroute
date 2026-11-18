//! `oxiroute top`: a live, read-only dashboard of listeners, pools, servers and in-flight requests, or one
//! snapshot for scripts. It reads `GET /api/v1/stats` and `GET /api/v1/requests` and computes rates itself.

use std::{io::IsTerminal as _, time::Duration};

#[allow(clippy::wildcard_imports)]
use super::*;
use rows::{Sort, View};

mod app;
mod draw;
mod keys;
mod once;
mod rates;
mod rows;
mod session;
mod source;
mod summary;
mod units;

#[cfg(test)]
mod tests;

const MIN_INTERVAL_MS: u64 = 1_000;

#[derive(Args)]
pub struct TopArgs {
    /// Refresh interval, for example 1s or 5s; not below 1s, the rate the server rebuilds statistics at.
    #[arg(long, value_name = "INTERVAL", default_value = "1s", value_parser = parse_interval)]
    interval: Duration,
    /// Print one snapshot as text and exit. This is also what happens without a terminal.
    #[arg(long)]
    once: bool,
    /// Print one snapshot as the management API's JSON document and exit.
    #[arg(long)]
    json: bool,
    /// The view to start in, or with `--once` the only one printed. Default: listeners.
    #[arg(long, value_enum)]
    view: Option<View>,
    /// Show only rows whose names contain this text, ignoring case.
    #[arg(long, value_name = "TEXT", default_value = "")]
    filter: String,
    /// Sort the starting view by this column title, for example `REQ/s`.
    #[arg(long, value_name = "COLUMN")]
    sort: Option<String>,
    /// Reverse the sort direction.
    #[arg(long)]
    reverse: bool,
}

fn parse_interval(value: &str) -> Result<Duration, String> {
    let milliseconds = parse_age_ms(value)?;
    if milliseconds < MIN_INTERVAL_MS {
        return Err("the interval must be at least 1s".into());
    }
    Ok(Duration::from_millis(milliseconds))
}

impl TopArgs {
    fn start_view(&self) -> View {
        self.view.unwrap_or(View::Listeners)
    }

    fn start_sort(&self) -> Result<Sort, CliError> {
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

    fn snapshot_views(&self) -> Vec<View> {
        self.view.map_or_else(
            || vec![View::Listeners, View::Pools, View::Servers],
            |view| vec![view],
        )
    }
}

pub(super) fn run(cli: &Cli, args: &TopArgs, client: Client) -> Result<(), CliError> {
    if client.endpoint.is_unix() {
        return Err(CliError::Local(
            "top reads the management API; registry sockets do not serve statistics".into(),
        ));
    }
    if args.json || matches!(cli.output, Output::Json) {
        return print_json_body(&client.get(source::STATS_PATH, true)?, cli.quiet);
    }
    let sort = args.start_sort()?;
    let first = source::sample(&client, args.start_view() == View::Requests)?;
    if args.once || !(io::stdin().is_terminal() && io::stdout().is_terminal()) {
        if !cli.quiet {
            let views = args.snapshot_views();
            let sorts = views.iter().map(|view| {
                if *view == args.start_view() {
                    sort
                } else {
                    view.default_sort()
                }
            });
            print!(
                "{}",
                once::render(&first, &views, &sorts.collect::<Vec<_>>(), &args.filter)
            );
        }
        return Ok(());
    }
    session::run(
        app::App::new(args.start_view(), sort, args.filter.clone(), args.interval),
        client,
        first,
    )
}
