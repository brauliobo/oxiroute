use std::time::Duration;

use crate::cli::{
    Cli, Command,
    context::Context,
    top::{TopArgs, rows::View},
};

fn parse(arguments: &[&str]) -> Result<Cli, clap::Error> {
    Cli::try_parse_process_from(["oxiroute"].into_iter().chain(arguments.iter().copied()))
}

fn top(arguments: &[&str]) -> TopArgs {
    match parse(&[&["top"], arguments].concat())
        .expect("valid arguments")
        .command
    {
        Command::Top(args) => args,
        _ => panic!("not the top command"),
    }
}

#[test]
fn the_defaults_refresh_every_second_in_the_listeners_view() {
    let args = top(&[]);
    assert_eq!(args.interval, Duration::from_secs(1));
    assert_eq!(args.start_view(), View::Listeners);
    assert!(!args.once && !args.json && args.filter.is_empty() && !args.reverse);
    let sort = args.start_sort().expect("default sort");
    assert_eq!((sort.column, sort.descending), (0, false));
}

#[test]
fn the_interval_is_not_below_the_servers_refresh_floor() {
    assert_eq!(top(&["--interval", "5s"]).interval, Duration::from_secs(5));
    assert!(parse(&["top", "--interval", "500ms"]).is_err());
    assert!(parse(&["top", "--interval", "fast"]).is_err());
}

#[test]
fn a_sort_column_is_matched_by_title_in_the_starting_view() {
    let args = top(&["--view", "servers", "--sort", "req/s"]);
    let sort = args.start_sort().expect("sort");
    assert_eq!(sort.column, View::Servers.column_named("REQ/s").unwrap());
    assert!(sort.descending);
    assert!(
        !top(&["--view", "servers", "--sort", "REQ/s", "--reverse"])
            .start_sort()
            .unwrap()
            .descending
    );
    let error = top(&["--sort", "bogus"])
        .start_sort()
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("unknown column bogus for the Listeners view") && error.contains("REQ/s"),
        "{error}"
    );
}

#[test]
fn without_a_view_a_snapshot_prints_listeners_pools_and_servers() {
    assert_eq!(
        top(&["--once"]).snapshot_views(),
        [View::Listeners, View::Pools, View::Servers]
    );
    assert_eq!(
        top(&["--view", "requests"]).snapshot_views(),
        [View::Requests]
    );
}

#[test]
fn a_registry_socket_is_refused_because_it_serves_no_statistics() {
    let cli =
        parse(&["--socket", "/run/oxiroute/status.sock", "top", "--once"]).expect("arguments");
    let context = Context::new(&cli.global, |_| Ok(()));
    let error = cli
        .subcommand()
        .run(&context)
        .expect_err("sockets serve no statistics");
    assert!(
        error
            .to_string()
            .contains("registry sockets do not serve statistics"),
        "{error}"
    );
}
