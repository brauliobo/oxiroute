use std::{fs, os::unix::fs::PermissionsExt as _, path::Path};

use tempfile::TempDir;

use super::{
    select::Selected,
    source::TokenSource,
    store::{Entry, Store},
};

const TOKEN: &str = "cdb85a91948758cfcb895216a3603c8fcd8aaf691f39f5fd82b5df15af14628e";

fn store(directory: &TempDir) -> Store {
    Store::at(directory.path().join("oxiroute").join("contexts"))
}

fn entry(endpoint: &str, token: Option<TokenSource>) -> Entry {
    Entry {
        endpoint: endpoint.to_owned(),
        token,
        timeout: Some("7s".into()),
    }
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).expect("metadata").permissions().mode() & 0o7777
}

#[test]
fn a_saved_file_is_private_and_round_trips() {
    let directory = TempDir::new().expect("directory");
    let store = store(&directory);
    let mut file = store.load().expect("an absent file is empty");
    file.contexts.insert(
        "prod".into(),
        entry(
            "http://192.0.2.10:9900",
            Some(TokenSource::File("/etc/oxiroute/prod.token".into())),
        ),
    );
    file.current = Some("prod".into());
    store.save(&file).expect("save");

    assert_eq!(mode(&store.path), 0o600);
    assert_eq!(mode(store.path.parent().expect("directory")), 0o700);
    let loaded = store.load().expect("load");
    assert_eq!(loaded.current.as_deref(), Some("prod"));
    assert_eq!(loaded.contexts["prod"].endpoint, "http://192.0.2.10:9900");
    assert_eq!(loaded.contexts["prod"].timeout.as_deref(), Some("7s"));
}

#[test]
fn a_file_with_a_loose_mode_is_refused() {
    let directory = TempDir::new().expect("directory");
    let store = store(&directory);
    store.save(&store.load().expect("empty")).expect("save");
    fs::set_permissions(&store.path, fs::Permissions::from_mode(0o644)).expect("chmod");

    let error = store
        .load()
        .expect_err("a readable file is refused")
        .to_string();
    assert!(
        error.contains("mode 0644") && error.contains("chmod 600"),
        "{error}"
    );
}

#[test]
fn a_token_written_into_the_file_is_refused_without_echoing_it() {
    let directory = TempDir::new().expect("directory");
    let store = store(&directory);
    store.save(&store.load().expect("empty")).expect("save");
    fs::write(
        &store.path,
        format!(
            r#"{{"schemaVersion":1,"contexts":{{"prod":{{"endpoint":"http://192.0.2.10:9900","token":"{TOKEN}"}}}}}}"#
        ),
    )
    .expect("write");

    let error = store
        .load()
        .expect_err("an inline token is refused")
        .to_string();
    assert!(error.contains("never stored"), "{error}");
    assert!(!error.contains(TOKEN), "{error}");
}

#[test]
fn unknown_fields_a_missing_current_and_bad_sources_are_refused() {
    let directory = TempDir::new().expect("directory");
    let store = store(&directory);
    store.save(&store.load().expect("empty")).expect("save");
    for (text, reason) in [
        (
            r#"{"schemaVersion":1,"contexts":{},"extra":1}"#,
            "not valid",
        ),
        (
            r#"{"schemaVersion":1,"current":"gone","contexts":{}}"#,
            "gone is not defined",
        ),
        (r#"{"schemaVersion":2,"contexts":{}}"#, "schemaVersion 2"),
        (
            r#"{"schemaVersion":1,"contexts":{"a":{"endpoint":"https://x"}}}"#,
            "endpoint must use http:// or unix:",
        ),
        (
            r#"{"schemaVersion":1,"contexts":{"a":{"endpoint":"http://x:1","token":{"file":"relative"}}}}"#,
            "absolute file path",
        ),
        (
            r#"{"schemaVersion":1,"contexts":{"a":{"endpoint":"http://x:1","timeout":"0s"}}}"#,
            "timeout",
        ),
    ] {
        fs::write(&store.path, text).expect("write");
        let error = store.load().expect_err(text).to_string();
        assert!(error.contains(reason), "{text}: {error}");
    }
}

#[test]
fn the_selection_names_a_known_context_by_request_or_by_the_current_one() {
    let directory = TempDir::new().expect("directory");
    let store = store(&directory);
    let mut file = store.load().expect("empty");
    file.contexts
        .insert("prod".into(), entry("http://192.0.2.10:9900", None));
    file.contexts
        .insert("staging".into(), entry("http://192.0.2.20:9900", None));
    file.current = Some("prod".into());
    store.save(&file).expect("save");

    let current = Selected::resolve_in(&store, None)
        .expect("current")
        .expect("selected");
    assert_eq!(current.name, "prod");
    let requested = Selected::resolve_in(&store, Some("staging"))
        .expect("named")
        .expect("selected");
    assert_eq!(requested.entry.endpoint, "http://192.0.2.20:9900");
    assert_eq!(requested.timeout().map(|limit| limit.as_secs()), Some(7));
    let error = Selected::resolve_in(&store, Some("missing"))
        .err()
        .expect("unknown")
        .to_string();
    assert!(error.contains("no context named missing"), "{error}");
}

#[test]
fn no_context_is_selected_without_a_file_or_a_current_one() {
    let directory = TempDir::new().expect("directory");
    assert!(
        Selected::resolve_in(&store(&directory), None)
            .expect("absent file")
            .is_none()
    );
}

#[test]
fn a_token_comes_from_a_file_a_variable_or_a_command_and_is_validated() {
    let directory = TempDir::new().expect("directory");
    let file = directory.path().join("token");
    fs::write(&file, format!("{TOKEN}\n")).expect("token file");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).expect("chmod");
    assert_eq!(
        TokenSource::File(file).read().expect("file").as_str(),
        TOKEN
    );

    let command = TokenSource::Command(vec!["echo".into(), TOKEN.into()]);
    assert_eq!(command.read().expect("command").as_str(), TOKEN);
    let short = TokenSource::Command(vec!["echo".into(), "short".into()]);
    assert!(
        short
            .read()
            .expect_err("short token")
            .to_string()
            .contains("5 characters")
    );
    let failing = TokenSource::Command(vec!["false".into()]);
    assert!(
        failing
            .read()
            .expect_err("failure")
            .to_string()
            .contains("failed")
    );
    let unset = TokenSource::Env("OXIROUTE_TEST_UNSET_TOKEN_VARIABLE".into());
    assert!(
        unset
            .read()
            .expect_err("unset")
            .to_string()
            .contains("not set")
    );
}

#[test]
fn a_token_source_shows_where_the_token_is_never_the_token() {
    let command = TokenSource::Command(vec!["pass".into(), "show".into(), "oxiroute".into()]);
    assert_eq!(
        (command.kind(), command.reference().as_str()),
        ("command", "pass")
    );
    let env = TokenSource::Env("PROD_TOKEN".into());
    assert_eq!(
        (env.kind(), env.reference().as_str()),
        ("env", "PROD_TOKEN")
    );
}
