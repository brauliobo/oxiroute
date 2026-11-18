use std::{fs, os::unix::fs::PermissionsExt as _};

use super::{store::OverlayError, *};
use crate::{AdministrativeState, HealthOverride, ServerAdminState};

fn server(pool: &str, name: &str, setting: Setting) -> OverlayEdit {
    OverlayEdit::Server {
        pool: pool.into(),
        server: name.into(),
        setting,
    }
}

fn store() -> (tempfile::TempDir, OverlayStore) {
    let directory = tempfile::tempdir().expect("directory");
    let store = OverlayStore::beside(&directory.path().join("oxiroute.kdl"));
    (directory, store)
}

#[test]
fn entries_are_sorted_whatever_the_order_of_the_edits() {
    let edits = [
        server("web", "b", Setting::Weight(Some(3))),
        server("api", "a", Setting::State(AdministrativeState::Drain)),
        server("web", "a", Setting::MaxConnections(Some(8))),
    ];
    let forward = Overlay::default().edited(edits.clone());
    let backward = Overlay::default().edited(edits.into_iter().rev());

    assert_eq!(forward, backward);
    let order = forward
        .servers
        .iter()
        .map(|entry| (entry.pool.as_str(), entry.server.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(order, [("api", "a"), ("web", "a"), ("web", "b")]);
}

#[test]
fn setting_a_default_removes_the_entry() {
    let overlay = Overlay::default()
        .edited([
            server("web", "a", Setting::State(AdministrativeState::Maintenance)),
            server("web", "a", Setting::Health(HealthOverride::Down)),
            server("web", "a", Setting::Checks(false)),
            server("web", "a", Setting::MaxConnections(Some(2))),
            server("web", "a", Setting::Weight(Some(5))),
            OverlayEdit::Listener {
                listener: "public".into(),
                state: AdministrativeState::Drain,
            },
        ])
        .edited([
            server("web", "a", Setting::State(AdministrativeState::Ready)),
            server("web", "a", Setting::Health(HealthOverride::Auto)),
            server("web", "a", Setting::Checks(true)),
            server("web", "a", Setting::MaxConnections(None)),
            server("web", "a", Setting::Weight(None)),
            OverlayEdit::Listener {
                listener: "public".into(),
                state: AdministrativeState::Ready,
            },
        ]);

    assert!(overlay.is_empty());
}

#[test]
fn the_overlay_fills_only_the_settings_the_runtime_leaves_at_the_default() {
    let overlay = Overlay::default().edited([
        server("web", "a", Setting::State(AdministrativeState::Drain)),
        server("web", "a", Setting::Weight(Some(9))),
        server("web", "b", Setting::MaxConnections(Some(4))),
    ]);
    let mut carried = ServerAdminState::unset("web", "a");
    carried.administrative = AdministrativeState::Maintenance;

    let merged = overlay.merged_with(&[carried]);

    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0].administrative, AdministrativeState::Maintenance);
    assert_eq!(merged[0].weight, Some(9));
    assert_eq!(merged[1].server, "b");
    assert_eq!(merged[1].max_connections, Some(4));
}

#[test]
fn the_overlay_can_disable_checks_but_never_re_enables_them() {
    let overlay = Overlay::default().edited([server("web", "a", Setting::Checks(false))]);
    let mut disabled = ServerAdminState::unset("web", "a");
    disabled.checks_enabled = false;

    assert!(!overlay.merged_with(&[])[0].checks_enabled);
    assert!(!Overlay::default().merged_with(&[disabled])[0].checks_enabled);
}

#[test]
fn promoted_capacities_leave_the_other_settings_of_the_server() {
    let overlay = Overlay::default()
        .edited([
            server("web", "a", Setting::MaxConnections(Some(4))),
            server("web", "a", Setting::Weight(Some(2))),
            server("web", "b", Setting::MaxConnections(Some(4))),
        ])
        .without_max_connections(&[("web".into(), "a".into()), ("web".into(), "b".into())]);

    assert_eq!(overlay.servers.len(), 1);
    assert_eq!(overlay.servers[0].weight, Some(2));
    assert_eq!(overlay.servers[0].max_connections, None);
}

#[test]
fn an_edit_is_durable_private_and_human_readable() {
    let (_directory, store) = store();
    assert!(store.revision().expect("revision").is_none());

    store
        .edit([server(
            "web",
            "a",
            Setting::State(AdministrativeState::Drain),
        )])
        .expect("edit");

    let text = fs::read_to_string(store.path()).expect("overlay text");
    assert_eq!(
        text,
        "{\n  \"schemaVersion\": 1,\n  \"servers\": [\n    {\n      \"pool\": \"web\",\n      \
         \"server\": \"a\",\n      \"state\": \"drain\"\n    }\n  ]\n}\n"
    );
    let mode = fs::metadata(store.path())
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    assert_eq!(store.load().expect("load").servers.len(), 1);
}

#[test]
fn the_revision_follows_the_bytes_and_an_emptied_overlay_removes_the_file() {
    let (_directory, store) = store();
    store
        .edit([server("web", "a", Setting::Weight(Some(2)))])
        .expect("first");
    let first = store.revision().expect("revision").expect("present");
    store
        .edit([server("web", "a", Setting::Weight(Some(3)))])
        .expect("second");
    let second = store.revision().expect("revision").expect("present");
    assert_ne!(first, second);

    store
        .edit([server("web", "a", Setting::Weight(None))])
        .expect("reset");

    assert!(!store.path().exists());
    assert!(store.load().expect("load").is_empty());
}

#[test]
fn an_unreadable_file_is_an_error_that_clear_still_removes() {
    let (_directory, store) = store();
    fs::write(store.path(), "{ not an overlay").expect("write");

    assert!(matches!(store.load(), Err(OverlayError::Invalid)));
    assert!(matches!(
        store.edit([server("web", "a", Setting::Weight(Some(2)))]),
        Err(OverlayError::Invalid)
    ));
    assert!(store.clear().expect("clear"));
    assert!(store.load().expect("load").is_empty());
}

#[test]
fn an_unknown_schema_version_or_field_is_refused() {
    let (_directory, store) = store();
    fs::write(store.path(), r#"{"schemaVersion": 2}"#).expect("write");
    assert!(matches!(store.load(), Err(OverlayError::UnknownSchema)));

    fs::write(store.path(), r#"{"schemaVersion": 1, "extra": true}"#).expect("write");
    assert!(matches!(store.load(), Err(OverlayError::Invalid)));
}

#[test]
fn a_hand_written_file_loads_in_canonical_form() {
    let (_directory, store) = store();
    fs::write(
        store.path(),
        r#"{"schemaVersion": 1, "servers": [
            {"pool": "web", "server": "b", "weight": 4},
            {"pool": "web", "server": "a", "state": "ready"},
            {"pool": "api", "server": "a", "state": "drain"}
        ]}"#,
    )
    .expect("write");

    let overlay = store.load().expect("load");

    let names = overlay
        .servers
        .iter()
        .map(|entry| format!("{}/{}", entry.pool, entry.server))
        .collect::<Vec<_>>();
    assert_eq!(names, ["api/a", "web/b"]);
}
