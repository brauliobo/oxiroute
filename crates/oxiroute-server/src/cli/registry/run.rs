use std::{
    fs::OpenOptions,
    io::{self, Write as _},
    os::unix::fs::OpenOptionsExt as _,
    path::Path,
};

use super::args::{RegistryCommand, RegistryTokenCommand};
use crate::{
    cli::{
        context::{Context, Execute},
        error::CliError,
        render,
    },
    registry::auth::{TokenFile, mint_token},
};

impl Execute for RegistryCommand {
    fn run(&self, _: &Context<'_>) -> Result<(), CliError> {
        let Self::Token {
            command:
                RegistryTokenCommand::Create {
                    tokens_file,
                    id,
                    pools,
                    name_prefix,
                    read,
                },
        } = self;
        let secret = create_token(tokens_file, id, pools, name_prefix.as_deref(), *read)
            .map_err(CliError::startup)?;
        render::emit(&format!("{secret}\n"));
        Ok(())
    }
}

/// Appends a scoped token to `file` and returns its secret, which is never stored.
pub(super) fn create_token(
    file: &Path,
    id: &str,
    pools: &[String],
    name_prefix: Option<&str>,
    read: bool,
) -> Result<String, CliError> {
    let scope = if read {
        "read".to_owned()
    } else {
        let prefix =
            name_prefix.map_or_else(String::new, |prefix| format!(" name_prefix={prefix}"));
        format!("register pools={}{prefix}", pools.join(","))
    };
    let local = |message: &str| CliError::Local(message.into());
    let (secret, line) =
        mint_token(id, &scope).map_err(|_| local("a random secret could not be generated"))?;
    let existing = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(_) => return Err(local("tokens file could not be read")),
    };
    let candidate = format!(
        "{existing}{}{line}\n",
        if existing.is_empty() || existing.ends_with('\n') {
            ""
        } else {
            "\n"
        }
    );
    TokenFile::parse(&candidate)
        .map_err(|_| local("the token id or scope is invalid or already used"))?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o640)
        .open(file)
        .and_then(|mut file| file.write_all(format!("{line}\n").as_bytes()))
        .map_err(|_| local("tokens file could not be written"))?;
    Ok(secret)
}
