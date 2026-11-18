use std::path::Path;

use zeroize::Zeroizing;

use crate::cli::{error::CliError, fs::read_regular_file};

const MAX_TOKEN_BYTES: usize = 512;

pub(in crate::cli) fn read_token(path: &Path) -> Result<Zeroizing<String>, CliError> {
    let bytes = Zeroizing::new(
        read_regular_file(path, MAX_TOKEN_BYTES + 2, true).map_err(|_| {
            CliError::Local("management token file could not be read securely".into())
        })?,
    );
    let mut token = Zeroizing::new(
        String::from_utf8(bytes.to_vec())
            .map_err(|_| CliError::Local("management token file is invalid".into()))?,
    );
    if token.ends_with("\r\n") {
        let length = token.len();
        token.truncate(length - 2);
    } else if token.ends_with('\n') {
        token.pop();
    }
    if token.len() < 32
        || token.len() > MAX_TOKEN_BYTES
        || !token.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(CliError::Local("management token file is invalid".into()));
    }
    Ok(token)
}
