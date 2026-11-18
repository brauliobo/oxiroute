use std::{
    fs::{self, OpenOptions},
    io::Write as _,
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

use openssl::rand::rand_bytes;

use crate::cli::{error::CliError, units::hex};

const TOKEN_BYTES: usize = 32;
pub(super) const TOKEN_CHARACTERS: usize = TOKEN_BYTES * 2;

fn new_token() -> Result<String, CliError> {
    let mut bytes = [0_u8; TOKEN_BYTES];
    rand_bytes(&mut bytes).map_err(|_| CliError::local("a random token could not be generated"))?;
    Ok(hex(&bytes))
}

fn io_error(action: &str, path: &Path, error: &std::io::Error) -> CliError {
    CliError::local(format!("cannot {action} {}: {error}", path.display()))
}

fn write_new(path: &Path, token: &str) -> Result<(), CliError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| io_error("create", path, &error))?;
    file.write_all(token.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| io_error("write", path, &error))
}

pub(super) fn create(path: &Path) -> Result<(), CliError> {
    write_new(path, &new_token()?)
}

fn replacement_of(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".new-{}", std::process::id()));
    PathBuf::from(name)
}

pub(super) fn rotate(path: &Path) -> Result<(), CliError> {
    let current = fs::symlink_metadata(path).map_err(|error| io_error("read", path, &error))?;
    if !current.is_file() {
        return Err(CliError::local(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    let replacement = replacement_of(path);
    write_new(&replacement, &new_token()?)?;
    let keep = |error: std::io::Error| {
        drop(fs::remove_file(&replacement));
        io_error("keep the owner and mode of", path, &error)
    };
    std::os::unix::fs::chown(&replacement, Some(current.uid()), Some(current.gid()))
        .map_err(keep)?;
    fs::set_permissions(
        &replacement,
        fs::Permissions::from_mode(current.mode() & 0o7777),
    )
    .map_err(keep)?;
    fs::rename(&replacement, path).map_err(|error| {
        drop(fs::remove_file(&replacement));
        io_error("replace", path, &error)
    })
}
