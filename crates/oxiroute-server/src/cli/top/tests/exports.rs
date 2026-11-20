use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::{
    fixture::{full, load, sample, snapshot, snapshot_document},
    harness::{PLAIN, app, running_full},
    server::{Fixture, Route},
};
use crate::cli::top::{
    export::{self, Kind, Plan},
    history::{History, Window},
    model::{Filter, Units},
    prefs::{Panel, PanelState, Prefs},
    views::View,
};

fn plan(view: View, window: Window) -> Plan {
    Plan {
        views: vec![view],
        sorts: BTreeMap::new(),
        filter: Filter::default(),
        window,
        units: Units::Bytes,
        prefs: Prefs::default(),
        top: 200,
    }
}

fn history() -> History {
    let mut history = History::default();
    history.push(full(snapshot(10_000, 100_000, &load(1_000, 0, 0)), 1));
    history.push(full(snapshot(12_000, 102_000, &load(3_000, 40, 60)), 3));
    history
}

fn json_of(view: View, window: Window) -> Value {
    let text = export::render(&history(), &plan(view, window), Kind::Json).expect("json");
    assert!(text.ends_with('\n') && text.matches('\n').count() == 1);
    serde_json::from_str(&text).expect("a JSON document")
}

#[test]
fn the_json_header_describes_the_sample_and_the_window() {
    let document = json_of(View::Listeners, Window::Short);
    assert_eq!(document["schemaVersion"], 2);
    assert_eq!(document["sampledAtUnixMs"], 12_000);
    assert_eq!(
        document["window"],
        json!({ "name": "2s", "ms": 2_000, "complete": true })
    );
    assert_eq!(document["instance"]["version"], "7.1.0");
    assert_eq!(document["instance"]["generation"], 4);
    assert_eq!(
        document["summary"]["connections"],
        json!({ "current": 7, "limit": 4_096 })
    );
    let rate = document["summary"]["rates"]["reqPerSec"]
        .as_f64()
        .expect("a rate");
    assert!((rate - 1_020.0).abs() < 0.5, "{rate}");
}

#[test]
fn the_json_body_lists_the_view_its_columns_and_its_rows() {
    let document = json_of(View::Listeners, Window::Short);
    assert_eq!(document["view"], "listeners");
    assert_eq!(
        document["sort"],
        json!({ "column": "name", "descending": false })
    );
    assert_eq!(document["filter"], "");
    assert_eq!(document["limit"], 200);
    assert_eq!(document["truncated"], false);
    assert_eq!(document["notice"], Value::Null);
    assert_eq!(document["other"], Value::Null);
    let columns = document["columns"].as_array().expect("columns");
    let req = columns
        .iter()
        .find(|column| column["key"] == "req")
        .expect("req");
    assert_eq!(
        *req,
        json!({ "key": "req", "title": "REQ/s", "unit": "perSecond", "visible": true })
    );
    let max = columns
        .iter()
        .find(|column| column["key"] == "max")
        .expect("max");
    assert_eq!(max["visible"], false);
    let rows = document["rows"].as_array().expect("rows");
    assert_eq!(rows[0]["key"], "edge");
    assert_eq!(rows[0]["depth"], 0);
    assert_eq!(rows[0]["parent"], Value::Null);
    assert_eq!(rows[0]["values"]["bind"], "socket:127.0.0.1:8080");
    assert!((rows[0]["values"]["req"].as_f64().expect("number") - 1_020.0).abs() < 0.5);
    assert_eq!(document["total"]["key"], "TOTAL");
}

#[test]
fn a_window_without_enough_samples_says_it_is_incomplete() {
    let document = json_of(View::Listeners, Window::Long);
    assert_eq!(document["window"]["complete"], false);
    assert_eq!(document["window"]["ms"], 60_000);
    let life = json_of(View::Listeners, Window::Life);
    assert_eq!(
        life["window"],
        json!({ "name": "life", "ms": Value::Null, "complete": true })
    );
}

#[test]
fn the_tree_rows_name_their_parents() {
    let document = json_of(View::Upstreams, Window::Short);
    let rows = document["rows"].as_array().expect("rows");
    let keys = rows
        .iter()
        .map(|row| row["key"].as_str().expect("key"))
        .collect::<Vec<_>>();
    assert_eq!(
        keys,
        [
            "api",
            "api/origin-c",
            "web",
            "web/origin-a",
            "web/origin-bb"
        ]
    );
    assert_eq!(rows[1]["parent"], "api");
    assert_eq!(rows[1]["depth"], 1);
    assert_eq!(rows[2]["parent"], Value::Null);
}

#[test]
fn the_screen_and_the_export_are_fed_by_the_same_rows() {
    for view in [
        View::Listeners,
        View::Upstreams,
        View::Clients,
        View::Conns,
        View::Requests,
        View::Rtmp,
        View::Generations,
    ] {
        let mut dashboard = running_full(view);
        dashboard.rearrange();
        let screen_keys = dashboard.arranged.as_ref().expect("rows").keys();
        let document = json_of(view, Window::Short);
        let exported = document["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .map(|row| row["key"].as_str().expect("key").to_owned())
            .collect::<Vec<_>>();
        assert_eq!(exported, screen_keys, "{view:?}");
    }
}

#[test]
fn the_filter_trims_rows_but_not_the_totals() {
    let mut printing = plan(View::Upstreams, Window::Short);
    printing.filter = Filter::parse("origin-bb");
    let text = export::render(&history(), &printing, Kind::Json).expect("json");
    let document: Value = serde_json::from_str(&text).expect("json");
    let rows = document["rows"].as_array().expect("rows");
    assert_eq!(
        rows.iter()
            .map(|row| row["key"].as_str().expect("key"))
            .collect::<Vec<_>>(),
        ["web", "web/origin-bb"]
    );
    assert_eq!(document["filter"], "origin-bb");
    let all = json_of(View::Upstreams, Window::Short);
    assert_eq!(document["total"], all["total"]);
}

#[test]
fn clients_export_the_pinned_rows_and_the_population() {
    let document = json_of(View::Clients, Window::Short);
    assert_eq!(document["other"]["key"], "other (1)");
    assert_eq!(document["total"]["values"]["reqs"], 2_775);
    assert_eq!(document["population"], json!({ "shown": 2, "of": 3 }));
    assert_eq!(document["truncated"], true);
    let rows = document["rows"].as_array().expect("rows");
    assert_eq!(rows[0]["key"], "192.0.2.10");
    assert_eq!(rows[0]["values"]["listener"], "edge");
}

#[test]
fn a_missing_feed_is_a_notice_in_the_document_and_no_rows() {
    let mut history = History::default();
    history.push(sample(snapshot(10_000, 100_000, &load(1_000, 0, 0))));
    history.push(sample(snapshot(12_000, 102_000, &load(3_000, 40, 60))));
    let text =
        export::render(&history, &plan(View::Clients, Window::Short), Kind::Json).expect("json");
    let document: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(document["rows"], json!([]));
    assert!(
        document["notice"]
            .as_str()
            .expect("a notice")
            .contains("waiting for the first sample")
    );
}

#[test]
fn the_csv_has_one_line_per_row_then_other_and_total() {
    let text =
        export::render(&history(), &plan(View::Clients, Window::Short), Kind::Csv).expect("csv");
    let lines = text.lines().collect::<Vec<_>>();
    assert!(
        lines[0].starts_with("kind,key,ip,conn,new,req,in,out"),
        "{}",
        lines[0]
    );
    assert!(
        lines[1].starts_with("row,192.0.2.10,192.0.2.10,3,"),
        "{}",
        lines[1]
    );
    assert!(lines[2].contains(r#""http,h2""#), "{}", lines[2]);
    assert!(lines[lines.len() - 2].starts_with("other,other (1),"));
    assert!(lines[lines.len() - 1].starts_with("total,TOTAL,"));
    let width = lines[0].split(',').count();
    assert!(lines.iter().all(|line| !line.is_empty()) && width > 8);
}

#[test]
fn the_text_says_what_the_rates_are_over() {
    let complete = export::render(
        &history(),
        &plan(View::Listeners, Window::Short),
        Kind::Text,
    )
    .expect("text");
    assert!(complete.contains("rates over a 2s window"));
    assert!(complete.starts_with("oxiroute 7.1.0  up 1m42s  generation #4 accepting"));
    assert!(complete.contains("\n\nListeners\nNAME"));
    assert!(complete.lines().any(|line| line.starts_with("TOTAL")));
    let filling =
        export::render(&history(), &plan(View::Listeners, Window::Long), Kind::Text).expect("text");
    assert!(filling.contains("rates over less than the 60s window: not enough samples yet"));
    let life =
        export::render(&history(), &plan(View::Listeners, Window::Life), Kind::Text).expect("text");
    assert!(life.contains("rates over the uptime of the server"));
}

#[test]
fn the_text_indents_children_and_follows_the_units_and_the_hidden_panels() {
    let mut printing = plan(View::Upstreams, Window::Short);
    printing.units = Units::Bits;
    printing.prefs.panels.set(Panel::Meters, PanelState::Hidden);
    let text = export::render(&history(), &printing, Kind::Text).expect("text");
    assert!(text.starts_with("Upstreams\nNAME"), "{text}");
    assert!(text.lines().any(|line| line.starts_with("  origin-c")));
    assert!(text.contains("Mb"));
    assert!(!text.contains("oxiroute 7.1.0"));
}

#[test]
fn the_hidden_columns_stay_out_of_every_format() {
    let mut printing = plan(View::Listeners, Window::Short);
    printing.prefs.edit_columns(View::Listeners, |columns| {
        let req = View::Listeners.column_named("req").expect("column");
        columns.toggle(View::Listeners, req, crate::cli::top::layout::Tier::Wide);
    });
    let text = export::render(&history(), &printing, Kind::Text).expect("text");
    assert!(!text.contains("REQ/s"));
    let csv = export::render(&history(), &printing, Kind::Csv).expect("csv");
    assert!(!csv.lines().next().expect("header").contains(",req,"));
    let document: Value =
        serde_json::from_str(&export::render(&history(), &printing, Kind::Json).expect("json"))
            .expect("json");
    let req = document["columns"]
        .as_array()
        .expect("columns")
        .iter()
        .find(|column| column["key"] == "req")
        .expect("req")
        .clone();
    assert_eq!(req["visible"], false);
    assert!(document["rows"][0]["values"].get("req").is_some());
}

#[test]
fn a_measurement_takes_two_samples_one_window_apart() {
    let routes = vec![Route::sequence(
        "/api/v1/stats",
        &[
            snapshot_document(10_000, 100_000, &load(1_000, 0, 0)),
            snapshot_document(12_000, 102_000, &load(3_000, 40, 60)),
        ],
    )];
    let fixture = Fixture::serve(routes);
    let printing = plan(View::Listeners, Window::Short);
    let started = std::time::Instant::now();
    let history = export::measure(&fixture.client(), &printing).expect("measure");
    assert!(started.elapsed() >= std::time::Duration::from_secs(2));
    assert_eq!(fixture.requested(), ["/api/v1/stats", "/api/v1/stats"]);
    let text = export::render(&history, &printing, Kind::Text).expect("text");
    assert!(text.contains("rates over a 2s window"));
    assert!(
        text.lines()
            .any(|line| line.starts_with("edge") && line.contains("1.0k")),
        "{text}"
    );
}

#[test]
fn the_life_window_needs_only_one_sample() {
    let fixture = Fixture::serve(vec![Route::ok(
        "/api/v1/stats",
        &snapshot_document(12_000, 102_000, &load(3_000, 40, 60)),
    )]);
    let printing = plan(View::Listeners, Window::Life);
    let history = export::measure(&fixture.client(), &printing).expect("measure");
    assert_eq!(fixture.requested().len(), 1);
    let text = export::render(&history, &printing, Kind::Text).expect("text");
    assert!(text.contains("rates over the uptime of the server"));
}

#[test]
fn the_dashboard_and_the_export_agree_on_the_row_count_with_a_filter() {
    let mut dashboard = app(View::Upstreams, Prefs::default(), PLAIN);
    dashboard.sample(Ok(full(snapshot(10_000, 100_000, &load(1_000, 0, 0)), 1)));
    dashboard.sample(Ok(full(snapshot(12_000, 102_000, &load(3_000, 40, 60)), 3)));
    dashboard.set_filter("web");
    dashboard.rearrange();
    let mut printing = plan(View::Upstreams, Window::Short);
    printing.filter = Filter::parse("web");
    let document: Value =
        serde_json::from_str(&export::render(&history(), &printing, Kind::Json).expect("json"))
            .expect("json");
    assert_eq!(
        document["rows"].as_array().expect("rows").len(),
        dashboard.arranged.as_ref().expect("rows").len()
    );
}
