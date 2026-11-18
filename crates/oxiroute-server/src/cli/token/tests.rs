use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
};

use tempfile::TempDir;

use super::file::{create, rotate};
use crate::rtmp::api::preflight_management_token;

#[test]
fn a_created_token_is_random_hex_that_only_its_owner_can_read_and_the_daemon_accepts() {
    let directory = TempDir::new().expect("directory");
    let path = directory.path().join("management.token");

    create(&path).expect("create");

    let token = fs::read_to_string(&path).expect("token");
    assert_eq!(token.len(), 64);
    assert!(
        token.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{token}"
    );
    assert_eq!(
        fs::metadata(&path).expect("metadata").mode() & 0o7777,
        0o600
    );
    preflight_management_token(&path).expect("the daemon accepts the file");
    assert!(create(&path).is_err());
    assert_eq!(fs::read_to_string(&path).expect("token"), token);
}

#[test]
fn rotating_replaces_the_token_in_place_keeping_the_mode_and_leaving_nothing_behind() {
    let directory = TempDir::new().expect("directory");
    let path = directory.path().join("management.token");
    create(&path).expect("create");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).expect("mode");
    let before = fs::read_to_string(&path).expect("token");

    rotate(&path).expect("rotate");

    let after = fs::read_to_string(&path).expect("token");
    assert_ne!(before, after);
    assert_eq!(after.len(), 64);
    assert_eq!(
        fs::metadata(&path).expect("metadata").mode() & 0o7777,
        0o400
    );
    preflight_management_token(&path).expect("the daemon accepts the file");
    assert_eq!(
        fs::read_dir(directory.path()).expect("directory").count(),
        1
    );
}

#[test]
fn only_an_existing_regular_file_can_be_rotated() {
    let directory = TempDir::new().expect("directory");
    assert!(rotate(&directory.path().join("absent.token")).is_err());
    assert!(rotate(directory.path()).is_err());
}
