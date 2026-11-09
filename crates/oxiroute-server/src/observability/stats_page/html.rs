//! The table that needs no script: the page itself until the page serves a script client, and the
//! fallback after.

use super::model::{ServerStats, StatsSnapshot, wire_name};
use crate::html::escape_html;

/// `admin` is the page prefix a form posts to and the active revision it carries; forms appear only when set.
pub(super) fn render(
    snapshot: &StatsSnapshot,
    refresh_ms: u64,
    admin: Option<(&str, &str)>,
) -> Vec<u8> {
    let mut body = String::from(
        "<!doctype html><html><head><meta charset=utf-8><title>OxiRoute statistics</title>",
    );
    body.push_str("<meta name=oxiroute-refresh-ms content=\"");
    body.push_str(&refresh_ms.to_string());
    body.push_str("\"><meta http-equiv=refresh content=\"");
    body.push_str(&format_refresh_seconds(refresh_ms));
    body.push_str("\"></head><body><h1>OxiRoute statistics</h1><table><thead><tr><th>Pool</th><th>Server</th><th>Administrative state</th><th>Health</th><th>Active</th>");
    if admin.is_some() {
        body.push_str("<th>Administration</th>");
    }
    body.push_str("</tr></thead><tbody>");
    for pool in &snapshot.pools {
        for server in &pool.servers {
            row(&mut body, &pool.name, server, admin);
        }
    }
    body.push_str("</tbody></table></body></html>");
    body.into_bytes()
}

fn row(body: &mut String, pool: &str, server: &ServerStats, admin: Option<(&str, &str)>) {
    body.push_str("<tr><td>");
    escape_html(body, pool);
    body.push_str("</td><td>");
    escape_html(body, &server.name);
    body.push_str("</td><td>");
    body.push_str(&wire_name(&server.administrative_state));
    body.push_str("</td><td>");
    body.push_str(&wire_name(&server.health));
    body.push_str("</td><td>");
    body.push_str(&server.traffic.sessions.current.to_string());
    body.push_str("</td>");
    if let Some((action, revision)) = admin {
        body.push_str("<td><form method=post action=\"");
        escape_html(body, action);
        body.push_str("\"><input type=hidden name=generation_revision value=\"");
        escape_html(body, revision);
        body.push_str("\"><input type=hidden name=pool value=\"");
        escape_html(body, pool);
        body.push_str("\"><input type=hidden name=server value=\"");
        escape_html(body, &server.name);
        body.push_str("\"><button name=state value=ready>Ready</button><button name=state value=drain>Drain</button><button name=state value=maintenance>Maintenance</button></form></td>");
    }
    body.push_str("</tr>");
}

fn format_refresh_seconds(refresh_ms: u64) -> String {
    if refresh_ms.is_multiple_of(1_000) {
        (refresh_ms / 1_000).to_string()
    } else {
        format!("{}.{:03}", refresh_ms / 1_000, refresh_ms % 1_000)
    }
}
