use std::{fs::File, io::Read as _, path::Path};

use http::{HeaderMap, HeaderName, HeaderValue};
use openssl::{memcmp, sha::sha256};
use rustix::fs::{self as rustix_fs, FileType, Mode, OFlags};
use zeroize::Zeroizing;

const MIN_TOKEN_BYTES: usize = 32;
const MAX_TOKEN_BYTES: usize = 512;
const MAX_TOKEN_FILE_BYTES: usize = MAX_TOKEN_BYTES + 2;

pub(crate) struct SecureBearerToken {
    digest: [u8; 32],
}

impl SecureBearerToken {
    pub(crate) fn new(token: &[u8]) -> Result<Self, SecureBearerTokenError> {
        if !(MIN_TOKEN_BYTES..=MAX_TOKEN_BYTES).contains(&token.len())
            || !token.iter().all(|byte| matches!(byte, 0x21..=0x7e))
        {
            return Err(SecureBearerTokenError::InvalidToken);
        }
        Ok(Self {
            digest: sha256(token),
        })
    }

    pub(crate) fn load(path: &Path) -> Result<Self, SecureBearerTokenError> {
        let descriptor = rustix_fs::open(
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|_| SecureBearerTokenError::Open)?;
        let before = rustix_fs::fstat(&descriptor).map_err(|_| SecureBearerTokenError::Read)?;
        if !FileType::from_raw_mode(before.st_mode).is_file() {
            return Err(SecureBearerTokenError::NotRegular);
        }
        if !matches!(before.st_mode & 0o7777, 0o400 | 0o600) {
            return Err(SecureBearerTokenError::InsecureMode);
        }
        let size = usize::try_from(before.st_size).map_err(|_| SecureBearerTokenError::TooLarge)?;
        if size > MAX_TOKEN_FILE_BYTES {
            return Err(SecureBearerTokenError::TooLarge);
        }

        let mut file = File::from(descriptor);
        let mut bytes = Zeroizing::new(Vec::with_capacity(size));
        file.by_ref()
            .take(u64::try_from(MAX_TOKEN_FILE_BYTES + 1).expect("token bound fits u64"))
            .read_to_end(&mut bytes)
            .map_err(|_| SecureBearerTokenError::Read)?;
        if bytes.len() > MAX_TOKEN_FILE_BYTES {
            return Err(SecureBearerTokenError::TooLarge);
        }
        let after = rustix_fs::fstat(&file).map_err(|_| SecureBearerTokenError::Read)?;
        if before.st_dev != after.st_dev
            || before.st_ino != after.st_ino
            || before.st_size != after.st_size
            || before.st_mode != after.st_mode
        {
            return Err(SecureBearerTokenError::Unstable);
        }
        trim_one_line_ending(&mut bytes);
        Self::new(&bytes)
    }

    pub(crate) fn authorizes(&self, authorization: &[u8]) -> bool {
        authorization
            .strip_prefix(b"Bearer ")
            .is_some_and(|candidate| memcmp::eq(&self.digest, &sha256(candidate)))
    }
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
pub(crate) enum SecureBearerTokenError {
    #[error("token file could not be securely opened")]
    Open,
    #[error("token file must be a regular no-follow file")]
    NotRegular,
    #[error("token file mode must be 0400 or 0600")]
    InsecureMode,
    #[error("token file exceeds the supported size")]
    TooLarge,
    #[error("token file could not be read")]
    Read,
    #[error("token file changed while it was read")]
    Unstable,
    #[error("token must be 32 to 512 visible ASCII bytes")]
    InvalidToken,
}

pub(crate) enum HeaderCardinality<'a> {
    Missing,
    Single(&'a HeaderValue),
    Duplicate,
}

pub(crate) fn single_header<'a>(
    headers: &'a HeaderMap,
    name: &HeaderName,
) -> HeaderCardinality<'a> {
    let mut values = headers.get_all(name).iter();
    match (values.next(), values.next()) {
        (None, _) => HeaderCardinality::Missing,
        (Some(value), None) => HeaderCardinality::Single(value),
        (Some(_), Some(_)) => HeaderCardinality::Duplicate,
    }
}

fn trim_one_line_ending(bytes: &mut Vec<u8>) {
    if bytes.ends_with(b"\r\n") {
        bytes.truncate(bytes.len() - 2);
    } else if bytes.ends_with(b"\n") {
        bytes.truncate(bytes.len() - 1);
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt as _};

    use http::{header::AUTHORIZATION, HeaderMap};
    use tempfile::tempdir;

    use super::{single_header, HeaderCardinality, SecureBearerToken};

    const TOKEN: &[u8] = b"0123456789abcdefghijklmnopqrstuv";

    #[test]
    fn token_policy_is_shared_across_inline_and_file_credentials() {
        let token = SecureBearerToken::new(TOKEN).expect("visible 32-byte token");
        assert!(token.authorizes(b"Bearer 0123456789abcdefghijklmnopqrstuv"));
        assert!(!token.authorizes(b"Basic 0123456789abcdefghijklmnopqrstuv"));
        assert!(SecureBearerToken::new(&TOKEN[..31]).is_err());

        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("token");
        fs::write(&path, [TOKEN, b"\r\n"].concat()).expect("write token");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("secure mode");
        assert!(SecureBearerToken::load(&path).is_ok());

        fs::write(&path, [TOKEN, b"\n\n"].concat()).expect("write invalid token");
        assert!(SecureBearerToken::load(&path).is_err());
    }

    #[test]
    fn single_header_distinguishes_missing_single_and_duplicate_values() {
        let mut headers = HeaderMap::new();
        assert!(matches!(
            single_header(&headers, &AUTHORIZATION),
            HeaderCardinality::Missing
        ));
        headers.append(AUTHORIZATION, "Bearer one".parse().unwrap());
        assert!(matches!(
            single_header(&headers, &AUTHORIZATION),
            HeaderCardinality::Single(value) if value == "Bearer one"
        ));
        headers.append(AUTHORIZATION, "Bearer two".parse().unwrap());
        assert!(matches!(
            single_header(&headers, &AUTHORIZATION),
            HeaderCardinality::Duplicate
        ));
    }
}
