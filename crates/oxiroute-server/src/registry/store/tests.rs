use std::{collections::BTreeMap, sync::Arc, thread};

use super::*;
use crate::{AdministrativeState, registry::record::AnnouncedState};

fn record(name: &str) -> Registration {
    Registration {
        pool: "render".into(),
        name: name.into(),
        address: "192.0.2.10:9000".parse().expect("address"),
        weight: 1,
        priority: 100,
        max_connections: None,
        labels: BTreeMap::new(),
        metadata: serde_json::Value::Null,
        ttl_ms: 15_000,
        announced_state: AnnouncedState::Ready,
        operator_state: AdministrativeState::Ready,
        registered_by: "token-a".into(),
    }
}

#[test]
fn a_missing_store_loads_empty_and_updates_bump_the_revision() {
    let dir = tempfile::tempdir().expect("dir");
    let store = Store::open(&dir.path().join("registry")).expect("open");
    assert_eq!(*store.load().expect("load"), StoredRegistry::default());

    let ((), seq) = store
        .update(|registry| (registry.upsert(record("a")), ()))
        .expect("update");

    assert_eq!(seq, 1);
    assert_eq!(store.load().expect("load").records, [record("a")]);
}

#[test]
fn an_unchanged_update_does_not_rewrite_or_bump() {
    let dir = tempfile::tempdir().expect("dir");
    let store = Store::open(dir.path()).expect("open");
    store
        .update(|registry| (registry.upsert(record("a")), ()))
        .expect("first");

    let ((), seq) = store
        .update(|registry| (registry.upsert(record("a")), ()))
        .expect("same");

    assert_eq!(seq, 1);
}

#[test]
fn the_store_is_private_and_keeps_a_backup_of_the_previous_file() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().expect("dir");
    let store = Store::open(&dir.path().join("registry")).expect("open");
    store
        .update(|registry| (registry.upsert(record("a")), ()))
        .expect("first");
    store
        .update(|registry| (registry.upsert(record("b")), ()))
        .expect("second");

    let mode = |name: &str| {
        fs::metadata(dir.path().join("registry").join(name))
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode("registrations.json"), 0o600);
    assert_eq!(mode(""), 0o700);
    let backup: StoredRegistry = serde_json::from_slice(
        &fs::read(dir.path().join("registry/registrations.json.bak")).expect("bak"),
    )
    .expect("backup parses");
    assert_eq!(backup.records.len(), 1);
}

#[test]
fn a_corrupt_store_is_reported_not_replaced() {
    let dir = tempfile::tempdir().expect("dir");
    let store = Store::open(dir.path()).expect("open");
    fs::write(dir.path().join("registrations.json"), b"{ truncated").expect("write");

    assert!(matches!(store.load(), Err(StoreError::Corrupt(_))));
    assert!(matches!(
        store.update(|registry| (registry.upsert(record("a")), ())),
        Err(StoreError::Corrupt(_))
    ));
    assert_eq!(
        fs::read(dir.path().join("registrations.json")).expect("read"),
        b"{ truncated"
    );
}

#[test]
fn concurrent_writers_never_lose_updates() {
    let dir = tempfile::tempdir().expect("dir");
    let store = Arc::new(Store::open(dir.path()).expect("open"));
    let writers = (0..4)
        .map(|writer| {
            let store = Arc::clone(&store);
            thread::spawn(move || {
                for index in 0..10 {
                    let name = format!("w{writer}-{index}");
                    store
                        .update(|registry| (registry.upsert(record(&name)), ()))
                        .expect("update");
                }
            })
        })
        .collect::<Vec<_>>();
    for writer in writers {
        writer.join().expect("writer");
    }

    let loaded = store.load().expect("load");
    assert_eq!(loaded.records.len(), 40);
    assert_eq!(loaded.seq, 40);
}

#[test]
fn reads_share_the_parsed_registry_until_the_file_is_replaced() {
    let dir = tempfile::tempdir().expect("dir");
    let store = Store::open(dir.path()).expect("open");
    store
        .update(|registry| (registry.upsert(record("a")), ()))
        .expect("write");

    let (first, second) = (store.load().expect("first"), store.load().expect("second"));
    assert!(
        Arc::ptr_eq(&first, &second),
        "an unchanged file is not parsed again"
    );

    let other = Store::open(dir.path()).expect("second handle, as another process");
    other
        .update(|registry| (registry.upsert(record("b")), ()))
        .expect("replace");
    let third = store.load().expect("third");

    assert_eq!(third.records.len(), 2, "a replaced file is read again");
    assert!(Arc::ptr_eq(&third, &store.load().expect("fourth")));
}

#[test]
fn removing_many_records_is_one_revision() {
    let dir = tempfile::tempdir().expect("dir");
    let store = Store::open(dir.path()).expect("open");
    store
        .update(|registry| {
            (0..5).for_each(|index| {
                registry.upsert(record(&format!("w{index}")));
            });
            (true, ())
        })
        .expect("seed");

    let (removed, seq) = store
        .update(|registry| {
            let keys = ["w1", "w3", "missing"].map(|name| record(name).key());
            let removed = registry.remove_all(&keys);
            (removed > 0, removed)
        })
        .expect("remove");

    assert_eq!((removed, seq), (2, 2));
    let names = store
        .load()
        .expect("load")
        .records
        .iter()
        .map(|record| record.name.clone())
        .collect::<Vec<_>>();
    assert_eq!(names, ["w0", "w2", "w4"]);
}
