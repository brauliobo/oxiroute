use std::{fs, os::unix::fs::PermissionsExt as _};

use tempfile::TempDir;

use super::{ConfigHistory, HistoryError, HistoryLimits};

fn history(directory: &TempDir) -> ConfigHistory {
    ConfigHistory::beside(&directory.path().join("oxiroute.kdl"))
}

fn limits(max_entries: usize, max_total_bytes: u64) -> HistoryLimits {
    HistoryLimits {
        max_entries,
        max_total_bytes,
    }
}

fn ids(history: &ConfigHistory) -> Vec<String> {
    history
        .list()
        .expect("list")
        .into_iter()
        .map(|entry| entry.id)
        .collect()
}

#[test]
fn entries_list_newest_first_and_read_back_exactly() {
    let directory = TempDir::new().expect("directory");
    let history = history(&directory);
    let first = history
        .record(b"one\n", 1_000, HistoryLimits::default())
        .expect("first");
    let second = history
        .record(b"two\n", 2_000, HistoryLimits::default())
        .expect("second");

    assert_eq!(ids(&history), [second.id.clone(), first.id.clone()]);
    let (entry, text) = history.read(&first.id).expect("read");
    assert_eq!(
        (entry.at_unix_ms, entry.bytes, text),
        (1_000, 4, b"one\n".to_vec())
    );
    assert_eq!(history.read(&second.id[..6]).expect("prefix").1, b"two\n");
}

#[test]
fn entries_are_private_and_leave_no_temporary_file() {
    let directory = TempDir::new().expect("directory");
    let history = history(&directory);
    let entry = history
        .record(b"secret = 1\n", 5, HistoryLimits::default())
        .expect("entry");

    let folder = directory.path().join("oxiroute.kdl.history");
    let mode =
        |path: &std::path::Path| fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
    assert_eq!(mode(&folder), 0o700);
    let names = fs::read_dir(&folder)
        .expect("list")
        .map(|item| {
            item.expect("item")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(names, [format!("0000000000005-{}.kdl", entry.id)]);
    assert_eq!(mode(&folder.join(&names[0])), 0o600);
}

#[test]
fn the_same_text_is_kept_once_with_its_latest_time() {
    let directory = TempDir::new().expect("directory");
    let history = history(&directory);
    history
        .record(b"a\n", 1, HistoryLimits::default())
        .expect("a");
    history
        .record(b"b\n", 2, HistoryLimits::default())
        .expect("b");
    history
        .record(b"a\n", 3, HistoryLimits::default())
        .expect("a again");

    let entries = history.list().expect("list");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].at_unix_ms, 3);
    assert_eq!(entries[1].at_unix_ms, 2);
}

#[test]
fn the_count_bound_drops_the_oldest_entries() {
    let directory = TempDir::new().expect("directory");
    let history = history(&directory);
    let mut recorded = Vec::new();
    for index in 0..7_u64 {
        recorded.push(
            history
                .record(
                    format!("text {index}\n").as_bytes(),
                    index + 1,
                    limits(3, u64::MAX),
                )
                .expect("record")
                .id,
        );
    }

    recorded.reverse();
    assert_eq!(ids(&history), recorded[..3]);
}

#[test]
fn the_byte_bound_drops_the_oldest_entries_and_never_exceeds_the_total() {
    let directory = TempDir::new().expect("directory");
    let history = history(&directory);
    for index in 0..10_u64 {
        history
            .record(
                format!("{index:0>9}\n").as_bytes(),
                index + 1,
                limits(100, 35),
            )
            .expect("record");
    }

    let entries = history.list().expect("list");
    assert_eq!(entries.len(), 3);
    assert!(entries.iter().map(|entry| entry.bytes).sum::<u64>() <= 35);
    assert_eq!(entries[0].at_unix_ms, 10);
}

#[test]
fn an_entry_bigger_than_the_total_bound_is_not_kept() {
    let directory = TempDir::new().expect("directory");
    let history = history(&directory);
    history
        .record(&[b'x'; 64], 1, limits(5, 32))
        .expect("record");
    assert!(history.list().expect("list").is_empty());
}

#[test]
fn an_id_must_be_long_enough_hexadecimal_and_known() {
    let directory = TempDir::new().expect("directory");
    let history = history(&directory);
    let entry = history
        .record(b"a\n", 1, HistoryLimits::default())
        .expect("a");

    assert!(matches!(history.read("abc"), Err(HistoryError::Invalid)));
    assert!(matches!(
        history.read("not-hex-id"),
        Err(HistoryError::Invalid)
    ));
    assert!(matches!(
        history.read("000000"),
        Err(HistoryError::NotFound)
    ));
    assert!(history.read(&entry.id).is_ok());
}

#[test]
fn a_missing_directory_is_an_empty_history() {
    let directory = TempDir::new().expect("directory");
    assert!(history(&directory).list().expect("list").is_empty());
}

#[test]
fn the_limits_follow_the_configuration_and_default_without_it() {
    use oxiroute_config::{ConfigDraft, ConfigHistory};
    use serde_json::json;

    let configured = serde_json::from_value::<ConfigDraft>(json!({
        "version": 1,
        "listeners": [],
        "config_history": {"max_entries": 7, "max_total_bytes": 5_242_880},
    }))
    .expect("draft")
    .validate()
    .expect("valid");
    let plain = serde_json::from_value::<ConfigDraft>(json!({"version": 1, "listeners": []}))
        .expect("draft")
        .validate()
        .expect("valid");

    assert_eq!(HistoryLimits::of(&configured), limits(7, 5_242_880));
    assert_eq!(
        HistoryLimits::of(&plain),
        limits(
            usize::try_from(ConfigHistory::DEFAULT_MAX_ENTRIES).unwrap(),
            ConfigHistory::DEFAULT_MAX_TOTAL_BYTES
        )
    );
    assert_eq!(HistoryLimits::of(&plain), HistoryLimits::default());
}
