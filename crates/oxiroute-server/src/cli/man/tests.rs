use super::ManArgs;
use crate::cli::{Cli, command::Command};

fn page(arguments: &[&str]) -> String {
    let cli = Cli::try_parse_process_from(arguments).expect("arguments parse");
    let Command::Man(args) = cli.subcommand() else {
        panic!("not a man command");
    };
    String::from_utf8(ManArgs::page(args).expect("page")).expect("UTF-8 page")
}

#[test]
fn the_page_names_the_command_lists_every_leaf_command_and_the_exit_status() {
    let page = page(&["oxiroute", "man"]);
    assert!(page.starts_with(".ie \\n(.g .ds Aq"), "{page}");
    assert!(page.contains(".TH oxiroute 1"), "{page}");
    assert!(page.contains(".SH COMMANDS"), "{page}");
    assert!(page.contains("oxiroute server drain"), "{page}");
    assert!(page.contains("oxiroute generation reload"), "{page}");
    assert!(page.contains(".SH \"EXIT STATUS\""), "{page}");
    assert!(page.contains("OXIROUTE_ENDPOINT"), "{page}");
    assert!(page.contains("/etc/oxiroute/management.token"), "{page}");
}

#[test]
fn the_alias_page_is_titled_and_synopsised_with_the_alias() {
    let page = page(&["oxiroute", "man", "--name", "oxr"]);
    assert!(page.contains(".TH oxr 1"), "{page}");
    assert!(page.contains("oxr server drain"), "{page}");
    assert!(!page.contains("oxiroute server drain"), "{page}");
}
