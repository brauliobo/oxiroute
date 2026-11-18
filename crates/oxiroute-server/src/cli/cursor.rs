//! Reads of the cursor-paged logs of the management API (events and audit records), oldest first.

use std::time::Duration;

use serde_json::Value;

use super::{error::CliError, net::Client, units::now_unix_ms};

pub(super) const PAGE_LIMIT: u16 = 1_000;

pub(super) fn items<'a>(page: &'a Value, key: &str) -> &'a [Value] {
    page.get(key)
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
}

pub(super) fn after(page: &Value, previous: u64) -> u64 {
    page.get("cursor")
        .and_then(Value::as_u64)
        .unwrap_or(previous)
}

pub(super) fn has_more(page: &Value) -> bool {
    page.get("hasMore").and_then(Value::as_bool) == Some(true)
}

pub(super) struct Recent {
    pub(super) items: Vec<Value>,
    pub(super) cursor: u64,
}

/// The retained entries of the last `since`, and the cursor after the last entry of the log.
pub(super) fn recent(
    client: &Client,
    key: &str,
    since: Duration,
    path: impl Fn(u64) -> String,
) -> Result<Recent, CliError> {
    let threshold =
        now_unix_ms().saturating_sub(u64::try_from(since.as_millis()).unwrap_or(u64::MAX));
    let mut items_found = Vec::new();
    let mut cursor = 0;
    loop {
        let page = client.get(&path(cursor), true)?.json()?;
        items_found.extend(
            items(&page, key)
                .iter()
                .filter(|item| {
                    item.get("timestampUnixMs").and_then(Value::as_u64) >= Some(threshold)
                })
                .cloned(),
        );
        let next = after(&page, cursor);
        if next == cursor || !has_more(&page) {
            return Ok(Recent {
                items: items_found,
                cursor: next,
            });
        }
        cursor = next;
    }
}
