use std::{fs, os::unix::fs::PermissionsExt as _};

use clap::{Parser as _, ValueEnum as _};
use tempfile::TempDir;

use super::{args::Kind, run::extension};
use crate::cli::{
    Cli, Command,
    config::{ComposeFormat, check},
};

fn init_args(words: &[&str]) -> super::InitArgs {
    let all = ["oxiroute", "init"]
        .into_iter()
        .chain(words.iter().copied());
    match Cli::try_parse_from(all).expect("CLI").command {
        Command::Init(args) => args,
        _ => panic!("init command"),
    }
}

#[test]
fn every_template_in_every_format_loads_validates_and_prepares() {
    let directory = TempDir::new().expect("directory");
    let token = directory.path().join("management.token");
    fs::write(&token, "a".repeat(64)).expect("token");
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).expect("mode");
    fs::create_dir(directory.path().join("public")).expect("public directory");
    for kind in Kind::value_variants() {
        for format in ComposeFormat::value_variants() {
            let name = kind
                .to_possible_value()
                .expect("name")
                .get_name()
                .to_owned();
            let format_name = format
                .to_possible_value()
                .expect("name")
                .get_name()
                .to_owned();
            let path = directory
                .path()
                .join(format!("{name}.{}", extension(*format)));
            fs::write(
                &path,
                init_args(&[&name, "--format", &format_name])
                    .render(directory.path())
                    .expect("render"),
            )
            .expect("write");
            let document = check::load(&path).unwrap_or_else(|error| panic!("{path:?}: {error}"));
            check::prepare(*document, false).unwrap_or_else(|error| panic!("{path:?}: {error}"));
        }
    }
}

#[test]
fn an_existing_file_is_kept_unless_forced() {
    let directory = TempDir::new().expect("directory");
    let file = directory.path().join("oxiroute.kdl");
    let file_arg = file.to_str().expect("path");
    let args = init_args(&["proxy", "--file", file_arg]);

    args.write(&file, "first").expect("create");
    let refused = args.write(&file, "second").expect_err("exists");
    assert!(refused.to_string().contains("--force"), "{refused}");
    assert_eq!(fs::read_to_string(&file).expect("file"), "first");

    init_args(&["proxy", "--file", file_arg, "--force"])
        .write(&file, "second")
        .expect("replace");
    assert_eq!(fs::read_to_string(&file).expect("file"), "second");
}
