use super::CompletionsArgs;
use crate::cli::{Cli, command::Command};

fn script(arguments: &[&str]) -> String {
    let cli = Cli::try_parse_process_from(arguments).expect("arguments parse");
    let Command::Completions(args) = cli.subcommand() else {
        panic!("not a completions command");
    };
    String::from_utf8(CompletionsArgs::script(args)).expect("UTF-8 script")
}

#[test]
fn bash_completes_commands_for_the_chosen_name() {
    let oxiroute = script(&["oxiroute", "completions", "bash"]);
    assert!(oxiroute.contains("_oxiroute()"), "{oxiroute}");
    assert!(oxiroute.contains("generation"), "{oxiroute}");
    assert!(oxiroute.contains("--timeout"), "{oxiroute}");
    let alias = script(&["oxiroute", "completions", "bash", "--name", "oxr"]);
    assert!(alias.contains("_oxr()"), "{alias}");
    assert!(!alias.contains("_oxiroute()"), "{alias}");
}

#[test]
fn zsh_and_fish_register_the_chosen_name() {
    assert!(script(&["oxiroute", "completions", "zsh"]).starts_with("#compdef oxiroute"));
    assert!(
        script(&["oxiroute", "completions", "zsh", "--name", "oxr"]).starts_with("#compdef oxr")
    );
    let fish = script(&["oxiroute", "completions", "fish", "--name", "oxr"]);
    assert!(fish.contains("complete -c oxr "), "{fish}");
}

#[test]
fn only_the_three_supported_shells_are_accepted() {
    assert!(Cli::try_parse_process_from(["oxiroute", "completions", "powershell"]).is_err());
    assert!(Cli::try_parse_process_from(["oxiroute", "completions"]).is_err());
}
