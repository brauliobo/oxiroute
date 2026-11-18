use clap::Parser as _;

use crate::cli::{Cli, Command};

fn path(arguments: &[&str]) -> String {
    let cli = Cli::try_parse_from(
        ["oxiroute", "requests"]
            .into_iter()
            .chain(arguments.iter().copied()),
    )
    .expect("CLI");
    let Command::Requests { command } = cli.subcommand() else {
        panic!("requests command")
    };
    command.path()
}

#[test]
fn requests_list_builds_one_filtered_query() {
    assert_eq!(
        path(&[
            "list",
            "--pool",
            "farm",
            "--older-than",
            "30s",
            "--phase",
            "queued"
        ]),
        "/api/v1/requests?pool=farm&phase=queued&min_age_ms=30000&limit=100"
    );
    assert_eq!(path(&["show", "rq-1"]), "/api/v1/requests?requestId=rq-1");
}
