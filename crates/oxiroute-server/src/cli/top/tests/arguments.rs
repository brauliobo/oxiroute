use std::time::Duration;

use clap::Parser;

use crate::cli::top::{args::TopArgs, history::Window, theme::ThemeName, views::View};

#[derive(Parser)]
struct Wrapper {
    #[command(flatten)]
    top: TopArgs,
}

fn parse(arguments: &[&str]) -> Result<TopArgs, clap::Error> {
    Wrapper::try_parse_from(["top"].into_iter().chain(arguments.iter().copied()))
        .map(|wrapper| wrapper.top)
}

#[test]
fn the_defaults_start_on_listeners_with_the_saved_choices() {
    let args = parse(&[]).expect("defaults");
    assert_eq!(args.interval, Duration::from_secs(1));
    assert_eq!(args.window, None);
    assert_eq!(args.top, 200);
    assert_eq!(args.start_view(), View::Listeners);
    assert_eq!(args.snapshot_views(), [View::Listeners, View::Upstreams]);
    assert!(!args.machine_readable());
    assert!(args.theme.is_none() && args.filter.is_empty());
}

#[test]
fn the_old_pool_and_server_names_open_the_upstreams() {
    for name in ["upstreams", "pools", "servers"] {
        assert_eq!(
            parse(&["--view", name]).expect("view").start_view(),
            View::Upstreams
        );
    }
    let args = parse(&["--view", "clients"]).expect("view");
    assert_eq!(args.snapshot_views(), [View::Clients]);
    assert!(parse(&["--view", "routes"]).is_err());
}

#[test]
fn the_interval_cannot_go_below_the_rate_the_server_rebuilds_statistics_at() {
    assert_eq!(
        parse(&["--interval", "5s"]).expect("interval").interval,
        Duration::from_secs(5)
    );
    assert!(parse(&["--interval", "500ms"]).is_err());
}

#[test]
fn the_window_and_theme_are_chosen_by_name() {
    let args = parse(&["--window", "60s", "--theme", "mono"]).expect("choices");
    assert_eq!(args.window, Some(Window::Long));
    assert_eq!(args.theme, Some(ThemeName::Mono));
    assert_eq!(
        parse(&["--window", "life"]).expect("window").window,
        Some(Window::Life)
    );
    assert!(parse(&["--window", "5s"]).is_err());
}

#[test]
fn json_and_csv_are_exclusive_and_either_is_machine_readable() {
    assert!(parse(&["--json", "--csv"]).is_err());
    assert!(parse(&["--json"]).expect("json").machine_readable());
    assert!(parse(&["--csv"]).expect("csv").machine_readable());
    assert!(!parse(&["--once"]).expect("once").machine_readable());
}

#[test]
fn the_number_of_clients_asked_for_stays_within_what_the_server_allows() {
    assert_eq!(parse(&["--top", "1000"]).expect("top").top, 1000);
    assert!(parse(&["--top", "0"]).is_err());
    assert!(parse(&["--top", "1001"]).is_err());
}

#[test]
fn the_start_sort_names_a_column_by_key_or_title_and_can_be_reversed() {
    let default = parse(&["--view", "clients"])
        .expect("args")
        .start_sort()
        .expect("sort");
    assert_eq!(
        default.column,
        View::Clients.column_named("in").expect("column")
    );
    assert!(default.descending);
    let by_title = parse(&["--view", "listeners", "--sort", "REQ/s"])
        .expect("args")
        .start_sort()
        .expect("sort");
    assert_eq!(
        by_title.column,
        View::Listeners.column_named("req").expect("column")
    );
    let reversed = parse(&["--view", "listeners", "--sort", "req", "--reverse"])
        .expect("args")
        .start_sort()
        .expect("sort");
    assert_eq!(reversed.descending, !by_title.descending);
    let error = parse(&["--sort", "nonsense"])
        .expect("args")
        .start_sort()
        .expect_err("unknown column")
        .to_string();
    assert!(
        error.contains("unknown column nonsense") && error.contains("NAME"),
        "{error}"
    );
}

#[test]
fn the_terminal_switches_default_off() {
    let args = parse(&[
        "--no-mouse",
        "--ascii",
        "--reset-layout",
        "--color",
        "never",
    ])
    .expect("args");
    assert!(args.terminal.no_mouse && args.terminal.ascii && args.terminal.reset_layout);
    let args = parse(&[]).expect("defaults");
    assert!(!args.terminal.no_mouse && !args.terminal.ascii && !args.terminal.reset_layout);
}
