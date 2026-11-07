use std::{
    fs,
    os::unix::fs::{PermissionsExt as _, symlink},
};

use http::{HeaderMap, header::AUTHORIZATION};
use rustix::fs as rustix_fs;
use tempfile::tempdir;

use super::{
    HeaderCardinality, SecureBearerToken, SecureBearerTokenError, same_file_snapshot, single_header,
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
