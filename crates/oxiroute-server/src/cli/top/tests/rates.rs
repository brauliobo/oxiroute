use super::fixture::{Load, snapshot};
use crate::cli::top::{
    rates::Sampling,
    rows::{Key, Sort, Table, View},
};

fn load(requests: u64, errors: u64, slow: u64) -> Load {
    Load {
        requests,
        errors,
        slow,
    }
}

fn table(
    view: View,
    before: Option<&crate::observability::stats_page::model::StatsSnapshot>,
    now: &crate::observability::stats_page::model::StatsSnapshot,
) -> Table {
    view.table(&Sampling::new(now, before), None)
}

fn cell<'a>(table: &'a Table, row: &str, column: &str) -> &'a crate::cli::top::rows::Cell {
    let index = table.view.column_named(column).expect("column");
    let row = table
        .rows
        .iter()
        .find(|candidate| candidate.search.starts_with(row))
        .expect("row");
    &row.cells[index]
}

#[test]
fn rates_are_counter_deltas_over_the_server_clock() {
    let before = snapshot(10_000, 100_000, &load(1_000, 0, 0));
    let now = snapshot(12_000, 102_000, &load(3_000, 0, 0));
    let listeners = table(View::Listeners, Some(&before), &now);
    assert_eq!(cell(&listeners, "edge", "REQ/s").key, Key::Number(1_000.0));
    assert_eq!(cell(&listeners, "edge", "REQ/s").text, "1.0k");
    assert_eq!(
        cell(&listeners, "edge", "IN/s").key,
        Key::Number(4_000_000.0)
    );
    assert_eq!(cell(&listeners, "edge", "ERR/s").key, Key::Number(0.0));
    let servers = table(View::Servers, Some(&before), &now);
    assert_eq!(
        cell(&servers, "web origin-a", "REQ/s").key,
        Key::Number(1_000.0)
    );
    assert_eq!(
        cell(&servers, "web origin-a", "OUT/s").key,
        Key::Number(4_000_000.0)
    );
}

#[test]
fn a_first_sample_has_no_rates_and_reads_latency_over_the_lifetime() {
    let now = snapshot(12_000, 102_000, &load(1_000, 0, 0));
    let listeners = table(View::Listeners, None, &now);
    assert_eq!(cell(&listeners, "edge", "REQ/s").key, Key::Missing);
    assert_eq!(cell(&listeners, "edge", "P50").text, "10ms");
    assert_eq!(cell(&listeners, "edge", "P99").text, "10ms");
}

#[test]
fn percentiles_follow_the_window_once_two_samples_exist() {
    let before = snapshot(10_000, 100_000, &load(1_000, 0, 0));
    let now = snapshot(12_000, 102_000, &load(1_100, 0, 5));
    let window = table(View::Listeners, Some(&before), &now);
    assert_eq!(cell(&window, "edge", "P50").text, "10ms");
    assert_eq!(cell(&window, "edge", "P99").text, "500ms");
    let lifetime = table(View::Listeners, None, &now);
    assert_eq!(cell(&lifetime, "edge", "P99").text, "10ms");
}

#[test]
fn a_restart_or_a_reset_counter_gives_no_rate() {
    let before = snapshot(10_000, 100_000, &load(3_000, 0, 0));
    let restarted = snapshot(12_000, 1_000, &load(10, 0, 0));
    let listeners = table(View::Listeners, Some(&before), &restarted);
    assert_eq!(cell(&listeners, "edge", "REQ/s").key, Key::Missing);
    assert_eq!(cell(&listeners, "edge", "P50").text, "10ms");
    let reset = snapshot(12_000, 102_000, &load(10, 0, 0));
    let servers = table(View::Servers, Some(&before), &reset);
    assert_eq!(cell(&servers, "web origin-a", "REQ/s").key, Key::Missing);
}

#[test]
fn a_late_sample_with_the_same_clock_is_not_a_window() {
    let before = snapshot(10_000, 100_000, &load(1_000, 0, 0));
    let same = snapshot(10_000, 100_000, &load(2_000, 0, 0));
    let listeners = table(View::Listeners, Some(&before), &same);
    assert_eq!(cell(&listeners, "edge", "REQ/s").key, Key::Missing);
}

#[test]
fn the_cache_hit_ratio_is_windowed_with_two_samples_and_lifetime_with_one() {
    let before = snapshot(10_000, 100_000, &load(1_000, 0, 0));
    let now = snapshot(12_000, 102_000, &load(2_000, 0, 0));
    let windowed = table(View::Listeners, Some(&before), &now);
    assert_eq!(cell(&windowed, "edge", "CACHE").text, "75%");
    assert_eq!(
        cell(&table(View::Listeners, None, &now), "edge", "CACHE").text,
        "75%"
    );
}

#[test]
fn server_state_columns_report_what_the_snapshot_says() {
    let now = snapshot(12_000, 102_000, &load(1_000, 20, 0));
    let servers = table(View::Servers, None, &now);
    assert_eq!(cell(&servers, "web origin-a", "STATUS").text, "up");
    assert_eq!(cell(&servers, "web origin-bb", "STATUS").text, "down");
    assert_eq!(cell(&servers, "api origin-c", "STATUS").text, "drain");
    assert_eq!(cell(&servers, "web origin-a", "5XX").text, "20");
    assert_eq!(cell(&servers, "web origin-a", "CONN").text, "2/50");
    assert_eq!(cell(&servers, "web origin-a", "CHKFAIL").text, "2");
    let pools = table(View::Pools, None, &now);
    assert_eq!(cell(&pools, "web", "UP").text, "2/2");
    assert_eq!(cell(&pools, "web", "QWAIT").text, "30ms");
    assert_eq!(cell(&pools, "web", "QUEUE").text, "1");
}

#[test]
fn rows_sort_by_column_with_missing_values_last_and_filter_by_name() {
    let before = snapshot(10_000, 100_000, &load(1_000, 0, 0));
    let now = snapshot(12_000, 102_000, &load(3_000, 0, 0));
    let servers = table(View::Servers, Some(&before), &now);
    let by_rate = Sort::for_column(View::Servers, View::Servers.column_named("REQ/s").unwrap());
    let sorted = servers.clone().arranged(by_rate, "");
    let names: Vec<_> = sorted
        .rows
        .iter()
        .map(|row| row.cells[1].text.as_str())
        .collect();
    assert_eq!(names, ["origin-a", "origin-bb", "origin-c"]);
    let ascending = servers.clone().arranged(
        Sort {
            descending: false,
            ..by_rate
        },
        "",
    );
    assert_eq!(ascending.rows[0].cells[1].text, "origin-bb");
    let filtered = servers.arranged(by_rate, "ORIGIN-B");
    assert_eq!(filtered.rows.len(), 1);
    assert_eq!(filtered.rows[0].cells[1].text, "origin-bb");
}
