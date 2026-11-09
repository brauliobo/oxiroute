//! The shell of the page and the table that needs no script inside it.

use super::{
    assets,
    model::{ServerStats, StatsSnapshot, wire_name},
};
use crate::html::escape_html;

/// The shell with its server-side values filled in. The table that needs no script is its content until
/// the client replaces it. `revision` is the active revision a form posts to `prefix`; forms appear only
/// when set.
pub(super) fn render(
    snapshot: &StatsSnapshot,
    refresh_ms: u64,
    prefix: &str,
    revision: Option<&str>,
) -> Vec<u8> {
    let mut assets_prefix = String::new();
    escape_html(&mut assets_prefix, prefix.trim_end_matches('/'));
    // The table goes last: names in it must never be read as tokens.
    assets::shell()
        .replace("/__stats_prefix__", &assets_prefix)
        .replace("__stats_refresh_ms__", &refresh_ms.to_string())
        .replace("__stats_refresh_s__", &format_refresh_seconds(refresh_ms))
        .replace("__stats_table__", &table(snapshot, prefix, revision))
        .into_bytes()
}

fn table(snapshot: &StatsSnapshot, prefix: &str, revision: Option<&str>) -> String {
    let admin = revision.map(|revision| (prefix, revision));
    let mut body = String::from(
        "<table><thead><tr><th>Pool</th><th>Server</th><th>Administrative state</th><th>Health</th><th>Active</th>",
    );
    if admin.is_some() {
        body.push_str("<th>Administration</th>");
    }
    body.push_str("</tr></thead><tbody>");
    for pool in &snapshot.pools {
        for server in &pool.servers {
            row(&mut body, &pool.name, server, admin);
        }
    }
    body.push_str("</tbody></table>");
    body
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
