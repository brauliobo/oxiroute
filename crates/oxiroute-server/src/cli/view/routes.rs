use serde_json::Value;

use super::read::{items, number, text};
use crate::cli::render::{Document, Field, Record};

pub(super) fn test(value: &Value) -> Document {
    Document::rows(
        "matching routes",
        items(value, "matches").iter().map(row).collect(),
    )
    .noting(unmatched(value))
}

fn row(route: &Value) -> Record {
    let pool = route.get("pool");
    vec![
        Field::text("listener", text(route, "listener")),
        Field::text("route", text(route, "route")),
        Field::text("action", text(route, "action")),
        Field::number("status", number(route, "status")),
        Field::text("pool", pool.and_then(|pool| text(pool, "name"))),
        Field::text("servers", pool.map(servers).as_deref()),
        Field::text(
            "rescue_pool",
            route.get("rescuePool").and_then(|pool| text(pool, "name")),
        ),
        Field::text("select_pools", select_pools(route).as_deref()),
    ]
}

fn servers(pool: &Value) -> String {
    let count = |key| number(pool, key).map_or_else(|| "?".to_owned(), |count| count.to_string());
    format!(
        "{}/{} available",
        count("availableEndpoints"),
        count("totalEndpoints")
    )
}

fn select_pools(route: &Value) -> Option<String> {
    let names = items(route, "selectPools")
        .iter()
        .filter_map(|pool| text(pool, "name"))
        .collect::<Vec<_>>();
    (!names.is_empty()).then(|| names.join(","))
}

fn unmatched(value: &Value) -> Option<String> {
    let names = items(value, "unmatched")
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    (!names.is_empty()).then(|| format!("no route on listeners {}", names.join(", ")))
}
