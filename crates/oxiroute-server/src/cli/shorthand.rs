use std::{
    ffi::{OsStr, OsString},
    path::Path,
};

use oxiroute_config_source::ConfigFormat;

/// `oxiroute CONFIG` stands for `oxiroute serve CONFIG` when CONFIG is a file or looks like a path; any other
/// unknown word is left for the parser to report with its suggestions.
pub(super) fn expand(mut arguments: Vec<OsString>, command: &clap::Command) -> Vec<OsString> {
    if let Some(at) = first_operand(&arguments, command)
        && names_a_config(&arguments[at], command)
    {
        arguments.insert(at, "serve".into());
    }
    arguments
}

fn first_operand(arguments: &[OsString], command: &clap::Command) -> Option<usize> {
    let mut index = 1;
    while let Some(argument) = arguments.get(index) {
        let bytes = argument.as_encoded_bytes();
        if bytes == b"--" {
            return (index + 1 < arguments.len()).then_some(index + 1);
        }
        if !bytes.starts_with(b"-") || bytes == b"-" {
            return Some(index);
        }
        index += if takes_separate_value(argument, command) {
            2
        } else {
            1
        };
    }
    None
}

fn takes_separate_value(argument: &OsStr, command: &clap::Command) -> bool {
    let Some(name) = argument.to_str().and_then(|text| text.strip_prefix("--")) else {
        return false;
    };
    !name.contains('=')
        && command
            .get_arguments()
            .any(|arg| arg.get_long() == Some(name) && arg.get_action().takes_values())
}

fn names_a_config(operand: &OsStr, command: &clap::Command) -> bool {
    let is_command = operand
        .to_str()
        .is_some_and(|name| name == "help" || command.find_subcommand(name).is_some());
    let path = Path::new(operand);
    !is_command
        && (path.is_file()
            || operand.as_encoded_bytes().contains(&b'/')
            || path.extension().is_some() && ConfigFormat::infer(path).is_ok())
}
