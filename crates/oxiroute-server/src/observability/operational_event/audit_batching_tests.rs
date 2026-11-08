use std::{
    fs,
    os::unix::fs::PermissionsExt,
    time::{Duration, Instant},
};

use tempfile::{TempDir, tempdir};

use super::*;

fn limits() -> AuditLimits {
    AuditLimits {
        max_records: 10_000,
        max_record_bytes: 4 * 1024,
        max_file_bytes: 8 * 1024,
        max_total_bytes: 1024 * 1024,
        max_rotated_files: 7,
    }
}

fn directory() -> TempDir {
    let directory = tempdir().expect("audit directory");
    fs::set_permissions(
        directory.path(),
        fs::Permissions::from_mode(AUDIT_ROOT_MODE),
    )
    .expect("secure audit directory");
    directory
}

fn batched(store: &AuditStore, operation: &str, result: AuditResult) -> u64 {
    store
        .append_with(
            AuditDurability::Batched,
            &AuditContext::generated(),
            AuditCategory::Control,
            operation,
            result,
            None,
        )
        .expect("batched record")
}

fn immediate(store: &AuditStore, operation: &str) -> u64 {
    store
        .append(
            &AuditContext::generated(),
            AuditCategory::Reload,
            operation,
            AuditResult::Succeeded,
            None,
        )
        .expect("immediate record")
}

fn active_ids(directory: &TempDir) -> Vec<u64> {
    let text = fs::read_to_string(directory.path().join(AUDIT_ACTIVE_FILE)).unwrap_or_default();
    text.lines()
        .map(|line| {
            serde_json::from_str::<AuditRecord>(line)
                .expect("audit line")
                .id
        })
        .collect()
}

fn stored_records(directory: &TempDir) -> usize {
    fs::read_dir(directory.path())
        .expect("audit directory")
        .map(|entry| {
            fs::read_to_string(entry.expect("entry").path())
                .unwrap_or_default()
                .lines()
                .count()
        })
        .sum()
}

fn alternate(store: &AuditStore, count: u64) {
    for index in 0..count {
        let result = if index % 2 == 0 {
            AuditResult::Succeeded
        } else {
            AuditResult::Failed
        };
        batched(store, "upstream_endpoint_health", result);
    }
}

#[test]
fn a_burst_of_one_kind_of_health_event_is_one_record_and_one_sync() {
    let directory = directory();
    let store = AuditStore::open(directory.path(), limits()).expect("open audit store");

    for _ in 0..1_000 {
        batched(&store, "upstream_endpoint_health", AuditResult::Succeeded);
    }
    store.flush().expect("flush");

    assert_eq!(store.page(0, 10, None, None).records.len(), 1);
    let metrics = store.metric_snapshot();
    assert_eq!(
        metrics.operation_counts[AuditCategory::Control.index()][AuditResult::Succeeded.index()],
        1_000
    );
    assert!(store.sync_count() <= 2, "{} syncs", store.sync_count());
    assert_eq!(active_ids(&directory), [1]);
}

#[test]
fn batched_records_that_differ_are_kept_in_order_across_rotation_and_reopen() {
    let directory = directory();
    let store = AuditStore::open(directory.path(), limits()).expect("open audit store");

    alternate(&store, 200);
    store.flush().expect("flush");

    assert!(
        store.status().rotated_files > 0,
        "the active file rotates by size"
    );
    let rotations = store.status().rotated_files;
    assert!(
        store.sync_count() <= rotations + 2,
        "{} syncs",
        store.sync_count()
    );
    drop(store);
    let reopened = AuditStore::open(directory.path(), limits()).expect("reopen audit store");
    let ids: Vec<_> = reopened
        .page(0, 1_000, None, None)
        .records
        .iter()
        .map(|record| record.id)
        .collect();
    assert_eq!(ids, (1..=200).collect::<Vec<_>>());
}

#[test]
fn an_immediate_record_persists_the_pending_batch_before_itself() {
    let directory = directory();
    let store = AuditStore::open(directory.path(), limits()).expect("open audit store");

    batched(&store, "upstream_endpoint_health", AuditResult::Succeeded);
    immediate(&store, "generation_reload");

    assert_eq!(active_ids(&directory), [1, 2]);
    assert!(store.sync_count() <= 2, "{} syncs", store.sync_count());
}

#[test]
fn retention_by_record_count_never_rewrites_the_files_while_running() {
    let directory = directory();
    let limits = AuditLimits {
        max_records: 10,
        ..limits()
    };
    let store = AuditStore::open(directory.path(), limits).expect("open audit store");

    alternate(&store, 50);
    store.flush().expect("flush");

    let page = store.page(0, 100, None, None);
    assert_eq!(page.records.first().map(|record| record.id), Some(41));
    assert_eq!(page.latest_cursor, 50);
    assert_eq!(
        stored_records(&directory),
        50,
        "the files are only trimmed when reopened"
    );
    drop(store);
    let reopened = AuditStore::open(directory.path(), limits).expect("reopen audit store");
    assert_eq!(reopened.page(0, 100, None, None).records.len(), 10);
    assert_eq!(stored_records(&directory), 10);
}

#[test]
fn the_writer_thread_commits_a_pending_batch_without_further_appends() {
    let directory = directory();
    let store = AuditStore::open(directory.path(), limits()).expect("open audit store");

    batched(&store, "upstream_endpoint_health", AuditResult::Succeeded);
    let deadline = Instant::now() + Duration::from_secs(10);
    while active_ids(&directory).is_empty() {
        assert!(
            Instant::now() < deadline,
            "the writer thread never committed the batch"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(active_ids(&directory), [1]);
}

#[test]
fn dropping_the_store_commits_the_pending_batch() {
    let directory = directory();
    let store = AuditStore::open(directory.path(), limits()).expect("open audit store");

    alternate(&store, 3);
    drop(store);

    assert_eq!(active_ids(&directory), [1, 2, 3]);
}

#[test]
fn health_events_are_batched_and_operator_actions_are_not() {
    assert_eq!(
        durability_for_event(EventName::UpstreamEndpointHealth),
        AuditDurability::Batched
    );
    assert_eq!(
        durability_for_event(EventName::UpstreamEndpointEjection),
        AuditDurability::Batched
    );
    assert_eq!(
        durability_for_event(EventName::RtmpConnect),
        AuditDurability::Batched
    );
    assert_eq!(
        durability_for_event(EventName::ConfigurationReload),
        AuditDurability::Immediate
    );
    assert_eq!(
        durability_for_event(EventName::ProcessShutdown),
        AuditDurability::Immediate
    );
}
