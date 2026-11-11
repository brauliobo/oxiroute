//! The fixture directory `tests/fixtures/reverse_http3_golden/`: one JSON document per area.
//!
//! A case belongs to the area whose prefix is the longest match for its id; `stats` holds the counters. Every
//! file stays below `MAX_LINES` so a re-recorded area is reviewable.

use std::{fs, path::PathBuf};

use serde_json::{Value, json};

use crate::golden::pretty;

const MAX_LINES: usize = 1000;
pub const STATS: &str = "stats";

const AREAS: &[(&str, &[&str])] = &[
    ("fixed", &["fixed/", "redirect/", "routing/"]),
    ("policy", &["policy/", "auth/", "path/"]),
    ("static", &["static/"]),
    (
        "static-conditional",
        &[
            "static/if-none-match",
            "static/if-modified-since",
            "static/if-match",
            "static/if-unmodified-since",
            "static/without-etag-if-modified-since",
        ],
    ),
    (
        "static-ranges",
        &["static/range", "static/odd-ranges", "static/if-range"],
    ),
    ("proxy", &["proxy/", "proxy-failure/", "retry/"]),
    (
        "proxy-response",
        &["proxy-response/", "body/", "content-length/"],
    ),
    ("cache", &["cache/"]),
    ("cache-purge", &["cache/purge"]),
    (
        "cache-bypass",
        &[
            "cache/authorization-bypasses",
            "cache/request-body-bypasses",
            "cache/post-is-not-cached",
            "cache/no-store-response",
            "cache/error-status-is-not-cached",
            "cache/response-trailers-are-not-cached",
            "cache/success-without-validators",
        ],
    ),
    ("errors", &["malformed/", "header-section/"]),
    ("protocol", &["wire/", "limits/", "access-log/"]),
];

fn directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reverse_http3_golden")
}

fn file(area: &str) -> PathBuf {
    directory().join(format!("{area}.json"))
}

fn area_of(id: &str) -> &'static str {
    AREAS
        .iter()
        .flat_map(|(area, prefixes)| prefixes.iter().map(move |prefix| (*area, *prefix)))
        .filter(|(_, prefix)| id.starts_with(prefix))
        .max_by_key(|(_, prefix)| prefix.len())
        .unwrap_or_else(|| panic!("case {id} belongs to no fixture area"))
        .0
}

fn read(path: &PathBuf) -> Value {
    let text =
        fs::read_to_string(path).unwrap_or_else(|_| panic!("{}; record it first", path.display()));
    serde_json::from_str(&text).expect("golden fixture JSON")
}

/// The recorded document: the cases of every area file and the counters.
pub fn load() -> Value {
    let cases = AREAS
        .iter()
        .flat_map(|(area, _)| {
            read(&file(area))["cases"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    json!({ "version": 1, "cases": cases, "stats": read(&file(STATS))["stats"] })
}

/// Writes the observed document, one file per area.
pub fn store(observed: &Value) {
    fs::create_dir_all(directory()).expect("fixture directory");
    for (area, _) in AREAS {
        let cases = observed["cases"].as_array().into_iter().flatten();
        let cases = cases.filter(|case| area_of(case["id"].as_str().expect("case id")) == *area);
        write(area, json!({ "cases": cases.collect::<Vec<_>>() }));
    }
    write(STATS, json!({ "stats": observed["stats"] }));
    eprintln!("recorded the golden fixture to {}", directory().display());
}

/// Replaces only the listed cases (and the counters when `STATS` is listed) by their observed values; a
/// `deviation` note of the recorded case is kept.
pub fn store_selected(observed: &Value, ids: &[&str]) {
    let mut document = load();
    let cases = document["cases"].as_array_mut().expect("cases");
    for id in ids.iter().filter(|id| **id != STATS) {
        let fresh = find(observed, id).unwrap_or_else(|| panic!("case {id} was not observed"));
        match cases.iter_mut().find(|case| case["id"] == *id) {
            Some(recorded) => *recorded = with_note(fresh, recorded),
            None => cases.push(fresh),
        }
    }
    if ids.contains(&STATS) {
        document["stats"] = observed["stats"].clone();
    }
    store(&document);
}

fn find(document: &Value, id: &str) -> Option<Value> {
    let cases = document["cases"].as_array()?;
    cases.iter().find(|case| case["id"] == id).cloned()
}

fn with_note(mut fresh: Value, recorded: &Value) -> Value {
    if let Some(note) = recorded.get("deviation") {
        fresh["deviation"] = note.clone();
    }
    fresh
}

fn write(area: &str, document: Value) {
    let text = format!("{}\n", pretty(&document, 0));
    let lines = text.lines().count();
    assert!(
        lines < MAX_LINES,
        "fixture area {area} has {lines} lines: split it in AREAS"
    );
    fs::write(file(area), text).expect("write a golden fixture file");
}
