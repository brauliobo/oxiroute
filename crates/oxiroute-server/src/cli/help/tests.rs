use clap::{Command, CommandFactory as _};

use crate::cli::Cli;

fn walk(command: &Command, path: &str, missing: &mut Vec<String>) {
    for argument in command.get_arguments() {
        let described = argument.get_help().is_some() || argument.get_long_help().is_some();
        if !argument.is_hide_set() && !described {
            missing.push(format!("{path} --{}", argument.get_id()));
        }
    }
    for subcommand in command.get_subcommands() {
        let name = format!("{path} {}", subcommand.get_name());
        if subcommand.get_name() != "help" {
            if subcommand.get_about().is_none() {
                missing.push(name.clone());
            }
            walk(subcommand, &name, missing);
        }
    }
}

#[test]
fn every_command_and_option_has_a_one_line_description() {
    let mut command = Cli::command();
    command.build();
    let mut missing = Vec::new();
    walk(&command, "oxiroute", &mut missing);
    assert!(missing.is_empty(), "undescribed:\n{}", missing.join("\n"));
}

#[test]
fn the_help_documents_every_exit_code_the_manual_page_documents() {
    for (code, meaning) in super::EXIT_CATEGORIES {
        assert!(
            super::OVERVIEW.contains(&format!("\n  {code}  {meaning}")),
            "the overview lacks `{code}  {meaning}`"
        );
    }
}

#[test]
fn the_top_level_help_leads_with_getting_started_and_the_long_help_adds_the_exit_status() {
    let mut command = Cli::command();
    let short = command.render_help().to_string();
    assert!(short.contains("Getting started:"), "{short}");
    assert!(!short.contains("Exit status"), "{short}");
    let long = command.render_long_help().to_string();
    assert!(long.contains("Exit status"), "{long}");
    assert!(
        long.contains("oxiroute server drain --pool web app-1"),
        "{long}"
    );
}

#[test]
fn every_command_family_ends_its_help_with_examples() {
    let command = Cli::command();
    for family in [
        "events",
        "audit",
        "requests",
        "server",
        "pool",
        "listener",
        "generation",
        "config",
        "context",
        "overlay",
        "tls",
        "rtmp",
        "registry",
        "import",
    ] {
        let mut subcommand = command.find_subcommand(family).expect(family).clone();
        let help = subcommand.render_help().to_string();
        assert!(help.contains("Example"), "{family}: {help}");
        assert!(
            help.contains(&format!("oxiroute {family} ")),
            "{family}: {help}"
        );
    }
}
