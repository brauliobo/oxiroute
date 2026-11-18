use clap::Parser as _;

use super::args::{AuditCommand, Category, Filter, Outcome};
use crate::{
    cli::{Cli, Command},
    observability::operational_event::{AuditCategory, AuditResult},
};

fn audit(words: &[&str]) -> Result<AuditCommand, clap::Error> {
    let mut all = vec!["oxiroute", "audit"];
    all.extend_from_slice(words);
    Cli::try_parse_from(all).map(|cli| match cli.command {
        Command::Audit { command } => command,
        _ => panic!("audit command"),
    })
}

fn names<T: clap::ValueEnum>() -> Vec<String> {
    T::value_variants()
        .iter()
        .filter_map(|variant| Some(variant.to_possible_value()?.get_name().to_owned()))
        .collect()
}

#[test]
fn the_filter_vocabulary_is_the_one_the_daemon_accepts() {
    for name in names::<Category>() {
        assert!(AuditCategory::parse(&name).is_some(), "{name}");
    }
    for name in names::<Outcome>() {
        assert!(AuditResult::parse(&name).is_some(), "{name}");
    }
}

#[test]
fn a_filter_names_its_category_and_result_in_the_query() {
    let Ok(AuditCommand::List { filter, .. }) =
        audit(&["list", "--category", "control", "--result", "rejected"])
    else {
        panic!("a list command");
    };
    assert_eq!(
        filter.path(7, 50),
        "/api/v1/audit?after=7&limit=50&category=control&result=rejected"
    );
    let Filter { category, result } = filter;
    assert!(category.is_some() && result.is_some());
}

#[test]
fn a_window_replaces_the_cursor_and_the_page_size() {
    assert!(audit(&["list", "--since", "1h"]).is_ok());
    assert!(audit(&["list", "--since", "1h", "--after", "3"]).is_err());
    assert!(audit(&["list", "--since", "1h", "--limit", "3"]).is_err());
    assert!(audit(&["tail", "--since", "1h", "--after", "3"]).is_err());
    assert!(audit(&["list", "--limit", "0"]).is_err());
    assert!(audit(&["tail", "--interval", "0s"]).is_err());
    assert!(audit(&["list", "--category", "everything"]).is_err());
}
