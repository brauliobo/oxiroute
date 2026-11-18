use std::{fmt::Display, path::Path};

use zeroize::Zeroizing;

use crate::cli::{error::CliError, fs::read_regular_file};

const MAX_TOKEN_BYTES: usize = 512;
const MIN_TOKEN_BYTES: usize = 32;

pub(in crate::cli) fn read_token(path: &Path) -> Result<Zeroizing<String>, CliError> {
    let refused = |reason: &dyn Display| {
        CliError::Local(format!(
            "management token file {}: {reason} (a token file is a regular file with mode 0400 or \
             0600 holding {MIN_TOKEN_BYTES} to {MAX_TOKEN_BYTES} visible ASCII characters)",
            path.display()
        ))
    };
    let bytes = Zeroizing::new(
        read_regular_file(path, MAX_TOKEN_BYTES + 2, true).map_err(|error| refused(&error))?,
    );
    let mut token = Zeroizing::new(
        String::from_utf8(bytes.to_vec()).map_err(|_| refused(&"is not valid UTF-8"))?,
    );
    if token.ends_with("\r\n") {
        let length = token.len();
        token.truncate(length - 2);
    } else if token.ends_with('\n') {
        token.pop();
    }
    if token.len() < MIN_TOKEN_BYTES
        || token.len() > MAX_TOKEN_BYTES
        || !token.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(refused(&format!(
            "holds a token of {} characters, or one with characters that are not visible ASCII",
            token.len()
        )));
    }
    Ok(token)
}
