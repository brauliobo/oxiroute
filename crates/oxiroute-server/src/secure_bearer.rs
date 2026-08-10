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
        if bytes.len() != size || !same_file_snapshot(&before, &after) {
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

fn same_file_snapshot(first: &rustix_fs::Stat, second: &rustix_fs::Stat) -> bool {
    first.st_dev == second.st_dev
        && first.st_ino == second.st_ino
        && first.st_mode == second.st_mode
        && first.st_size == second.st_size
        && first.st_mtime == second.st_mtime
        && first.st_mtime_nsec == second.st_mtime_nsec
        && first.st_ctime == second.st_ctime
        && first.st_ctime_nsec == second.st_ctime_nsec
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::{PermissionsExt as _, symlink},
    };

    use http::{HeaderMap, header::AUTHORIZATION};
    use rustix::fs as rustix_fs;
    use tempfile::tempdir;

    use super::{
        HeaderCardinality, SecureBearerToken, SecureBearerTokenError, same_file_snapshot,
        single_header,
    };

    const TOKEN: &[u8] = b"0123456789abcdefghijklmnopqrstuv";

    #[test]
    fn token_authorization_requires_an_exact_bearer_credential() {
        let token = SecureBearerToken::new(TOKEN).expect("visible 32-byte token");
        assert!(token.authorizes(b"Bearer 0123456789abcdefghijklmnopqrstuv"));
        assert!(!token.authorizes(b"Basic 0123456789abcdefghijklmnopqrstuv"));
        assert!(!token.authorizes(b"bearer 0123456789abcdefghijklmnopqrstuv"));
        assert!(!token.authorizes(b"Bearer 0123456789abcdefghijklmnopqrstuX"));
        assert!(!token.authorizes(b"Bearer 0123456789abcdefghijklmnopqrstuv "));
    }

    #[test]
    fn token_bounds_apply_to_inline_and_file_credentials() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("token");
        for (length, accepted) in [(31, false), (32, true), (512, true), (513, false)] {
            let token = vec![b'x'; length];
            assert_eq!(SecureBearerToken::new(&token).is_ok(), accepted, "{length}");

            fs::write(&path, [token.as_slice(), b"\n"].concat()).expect("write token");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("secure mode");
            assert_eq!(SecureBearerToken::load(&path).is_ok(), accepted, "{length}");
        }
    }

    #[test]
    fn token_file_accepts_one_line_ending_only() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("token");
        for (ending, accepted) in [
            (b"".as_slice(), true),
            (b"\n".as_slice(), true),
            (b"\r\n".as_slice(), true),
            (b"\n\n".as_slice(), false),
            (b"\r\n\n".as_slice(), false),
        ] {
            fs::write(&path, [TOKEN, ending].concat()).expect("write token");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("secure mode");
            assert_eq!(
                SecureBearerToken::load(&path).is_ok(),
                accepted,
                "{ending:?}"
            );
        }
    }

    #[test]
    fn token_file_accepts_only_private_regular_no_follow_files() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("token");
        fs::write(&path, TOKEN).expect("write token");
        for (mode, accepted) in [(0o400, true), (0o600, true), (0o440, false), (0o640, false)] {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("token mode");
            assert_eq!(SecureBearerToken::load(&path).is_ok(), accepted, "{mode:o}");
        }

        let link = directory.path().join("token-link");
        symlink(&path, &link).expect("token symlink");
        assert!(matches!(
            SecureBearerToken::load(&link),
            Err(SecureBearerTokenError::Open)
        ));
        assert!(matches!(
            SecureBearerToken::load(directory.path()),
            Err(SecureBearerTokenError::NotRegular)
        ));
    }

    #[test]
    fn stable_snapshot_rejects_same_length_replacement_and_timestamp_changes() {
        let directory = tempdir().expect("tempdir");
        let first_path = directory.path().join("first");
        let second_path = directory.path().join("second");
        fs::write(&first_path, TOKEN).expect("first token");
        fs::write(&second_path, [b'x'; TOKEN.len()]).expect("replacement token");
        let first = rustix_fs::stat(&first_path).expect("first stat");
        let replacement = rustix_fs::stat(&second_path).expect("replacement stat");
        assert_eq!(first.st_size, replacement.st_size);
        assert!(!same_file_snapshot(&first, &replacement));

        let mut changed = first;
        changed.st_mtime_nsec ^= 1;
        assert!(!same_file_snapshot(&first, &changed));
        changed = first;
        changed.st_ctime_nsec ^= 1;
        assert!(!same_file_snapshot(&first, &changed));
        assert!(same_file_snapshot(&first, &first));
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
