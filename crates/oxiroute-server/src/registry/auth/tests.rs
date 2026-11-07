use super::*;

fn file(lines: &[&str]) -> TokenFile {
    TokenFile::parse(&lines.join("\n")).expect("token file")
}

#[test]
fn minted_tokens_authenticate_with_the_scope_of_their_line() {
    let (secret, line) =
        mint_token("gpu-agent", "register pools=render,voice name_prefix=gpu-").expect("mint");
    let tokens = file(&[&line]);

    let principal = tokens
        .authenticate(secret.as_bytes())
        .expect("authenticated");

    assert_eq!(principal.id, "gpu-agent");
    assert_eq!(
        principal.scope,
        Scope::Register {
            pools: vec!["render".into(), "voice".into()],
            name_prefix: Some("gpu-".into())
        }
    );
    assert!(tokens.authenticate(b"another-secret").is_none());
    assert_eq!(secret.len(), 64);
    assert!(!line.contains(&secret));
}

#[test]
fn register_scope_is_limited_to_its_pools_and_name_prefix() {
    let principal = Principal {
        id: "a".into(),
        scope: Scope::Register {
            pools: vec!["render".into()],
            name_prefix: Some("gpu-".into()),
        },
    };

    assert!(principal.check_register("render", "gpu-1").is_ok());
    assert_eq!(
        principal.check_register("voice", "gpu-1"),
        Err(RegistryError::ScopeDenied)
    );
    assert_eq!(
        principal.check_register("render", "cpu-1"),
        Err(RegistryError::ScopeDenied)
    );
    assert_eq!(principal.check_read(), Err(RegistryError::ScopeDenied));
    assert_eq!(principal.check_admin(), Err(RegistryError::ScopeDenied));
}

#[test]
fn read_scope_reads_but_cannot_register() {
    let principal = Principal {
        id: "r".into(),
        scope: Scope::Read,
    };

    assert!(principal.check_read().is_ok());
    assert_eq!(
        principal.check_register("render", "gpu"),
        Err(RegistryError::ScopeDenied)
    );
}

#[test]
fn malformed_lines_and_duplicate_ids_are_rejected_with_their_line_number() {
    let digest = "00".repeat(32);
    for (text, line) in [
        (format!("ok {digest} read\nbad"), 2),
        (format!("a {digest} write"), 1),
        (format!("a {digest} register"), 1),
        (format!("a {digest} register pools=x extra=1"), 1),
        ("a nothex read".to_owned(), 1),
        (format!("a {digest} read\na {digest} read"), 2),
    ] {
        assert!(
            matches!(TokenFile::parse(&text), Err(TokenFileError::InvalidLine(found)) if found == line),
            "{text}"
        );
    }
    assert!(TokenFile::parse(&format!("# comment\n\na {digest} read\n")).is_ok());
}

#[test]
fn the_store_reloads_changes_and_keeps_the_last_good_set_on_a_broken_edit() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("tokens");
    let (first, line) = mint_token("first", "read").expect("mint");
    fs::write(&path, format!("{line}\n")).expect("write");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("mode");
    let store = TokenStore::open(&path).expect("open");
    assert!(store.authenticate(first.as_bytes()).is_some());

    let (second, line) = mint_token("second", "read").expect("mint");
    fs::write(&path, format!("{line}\n")).expect("rewrite");
    assert!(store.authenticate(first.as_bytes()).is_none());
    assert!(store.authenticate(second.as_bytes()).is_some());

    fs::write(&path, "garbage\n").expect("break");
    assert!(store.authenticate(second.as_bytes()).is_some());
}

#[test]
fn a_group_or_world_writable_token_file_is_refused() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("tokens");
    fs::write(&path, "").expect("write");
    for mode in [0o660, 0o666, 0o604] {
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("mode");
        assert!(
            matches!(TokenFile::load(&path), Err(TokenFileError::InsecureMode)),
            "{mode:o}"
        );
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).expect("mode");
    assert!(TokenFile::load(&path).is_ok());
}
