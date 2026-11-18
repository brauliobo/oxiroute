use std::{
    fmt::Display,
    path::PathBuf,
    process::{Command, Stdio},
};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::cli::{
    error::CliError,
    net::{read_token, valid_token},
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) enum TokenSource {
    File(PathBuf),
    Env(String),
    Command(Vec<String>),
}

impl TokenSource {
    pub(super) const fn kind(&self) -> &'static str {
        match self {
            Self::File(_) => "file",
            Self::Env(_) => "env",
            Self::Command(_) => "command",
        }
    }

    pub(super) fn reference(&self) -> String {
        match self {
            Self::File(path) => path.display().to_string(),
            Self::Env(name) => name.clone(),
            Self::Command(argv) => argv.first().cloned().unwrap_or_default(),
        }
    }

    pub(super) fn is_well_formed(&self) -> bool {
        match self {
            Self::File(path) => path.is_absolute(),
            Self::Env(name) => is_variable_name(name),
            Self::Command(argv) => argv.first().is_some_and(|program| !program.is_empty()),
        }
    }

    pub(crate) fn read(&self) -> Result<Zeroizing<String>, CliError> {
        match self {
            Self::File(path) => read_token(path),
            Self::Env(name) => {
                let value = std::env::var(name).map_err(|_| {
                    CliError::Local(format!(
                        "the context takes its token from the environment variable {name}, which is not set"
                    ))
                })?;
                valid_token(Zeroizing::new(value), &|reason: &dyn Display| {
                    CliError::Local(format!("management token in {name}: {reason}"))
                })
            }
            Self::Command(argv) => run(argv),
        }
    }
}

fn run(argv: &[String]) -> Result<Zeroizing<String>, CliError> {
    let program = argv.first().map(String::as_str).unwrap_or_default();
    let refused = |reason: &dyn Display| {
        CliError::Local(format!("management token from `{program}`: {reason}"))
    };
    let output = Command::new(program)
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| refused(&format!("cannot run the command: {error}")))?;
    let stdout = Zeroizing::new(output.stdout);
    if !output.status.success() {
        return Err(refused(&format!(
            "the command failed with {}",
            output.status
        )));
    }
    let token = String::from_utf8(stdout.to_vec())
        .map_err(|_| refused(&"printed bytes that are not UTF-8"))?;
    valid_token(Zeroizing::new(token), &refused)
}

pub(super) fn is_variable_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}
