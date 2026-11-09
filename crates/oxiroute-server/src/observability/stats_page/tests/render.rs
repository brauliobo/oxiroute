use super::fixture::{API, WEB, fixture};
use crate::observability::stats_page::{
    builder::build_snapshot,
    csv, html,
    model::{ListenerState, ServerStatus, StatsSnapshot},
};

fn snapshot() -> StatsSnapshot {
    build_snapshot(&fixture().generations, 1_000).expect("snapshot")
}

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).expect("UTF-8")
}

#[test]
fn the_csv_header_names_the_haproxy_columns_the_proxy_can_fill() {
    let csv = text(csv::render(&snapshot()));
    assert_eq!(
        csv.lines().next().unwrap(),
        "# pxname,svname,qcur,scur,slim,stot,bin,bout,ereq,econ,eresp,wretr,status,weight,chkfail,lastchg,\
         downtime,type,check_status,hrsp_1xx,hrsp_2xx,hrsp_3xx,hrsp_4xx,hrsp_5xx,hrsp_other,req_tot,qtime,\
         rtime,ttime"
    );
    let width = csv.lines().next().unwrap().split(',').count();
    assert!(
        csv.lines().all(|line| line.split(',').count() == width),
        "{csv}"
    );
}

#[test]
fn csv_rows_leave_unknown_cells_empty_and_never_report_them_as_zero() {
    let mut snapshot = snapshot();
    let front = snapshot
        .listeners
        .iter_mut()
        .find(|listener| listener.name == "front")
        .unwrap();
    front.state = ListenerState::Listening;
    let csv = text(csv::render(&snapshot));
    let server = [
        "sp-api", "origin-c", "", "0", "", "0", "0", "0", "", "0", "0", "0", "no check", "1", "0",
        "", "", "2", "", "0", "0", "0", "0", "0", "0", "", "", "", "",
    ];
    assert!(csv.lines().any(|line| line == server.join(",")), "{csv}");
    let mut frontend = vec![
        "front", "FRONTEND", "", "0", "", "0", "0", "0", "", "", "", "", "OPEN", "", "", "", "",
        "0",
    ];
    frontend.extend([""; 11]);
    assert!(csv.lines().any(|line| line == frontend.join(",")), "{csv}");
    let backend = csv
        .lines()
        .find(|line| line.starts_with("sp-api,BACKEND,"))
        .expect("backend");
    let cells: Vec<_> = backend.split(',').collect();
    assert_eq!(
        (cells[2], cells[12], cells[13], cells[17]),
        ("0", "UP", "", "1")
    );
    assert_eq!(cells[26], "", "no queue wait observed yet, so no average");
}

#[test]
fn csv_statuses_use_haproxy_words_and_outage_times_are_seconds() {
    let mut snapshot = snapshot();
    let server = &mut snapshot
        .pools
        .iter_mut()
        .find(|pool| pool.name == API)
        .unwrap()
        .servers[0];
    server.status = ServerStatus::Maint;
    server.last_transition_at_unix_ms = Some(snapshot.sampled_at_unix_ms - 125_000);
    server.downtime_ms = Some(61_999);
    server.check.last_checked_at_unix_ms = Some(1);
    let csv = text(csv::render(&snapshot));
    let line = csv
        .lines()
        .find(|line| line.starts_with("sp-api,origin-c,"))
        .unwrap();
    let cells: Vec<_> = line.split(',').collect();
    assert_eq!(
        (cells[12], cells[15], cells[16], cells[18]),
        ("MAINT", "125", "61", "ok")
    );
}

#[test]
fn csv_quotes_delimiters_and_defuses_spreadsheet_formulas() {
    let mut snapshot = snapshot();
    let pool = snapshot
        .pools
        .iter_mut()
        .find(|pool| pool.name == WEB)
        .unwrap();
    pool.name = "a,\"b\"".into();
    pool.servers[0].name = "=HYPERLINK(\"http://example.com\")".into();
    pool.servers[1].name = "@SUM(1)\nrow".into();
    snapshot.listeners[0].name = "+cmd".into();
    let csv = text(csv::render(&snapshot));
    assert!(csv.contains("\"a,\"\"b\"\"\",BACKEND,"), "{csv}");
    assert!(
        csv.contains("\"a,\"\"b\"\"\",\"'=HYPERLINK(\"\"http://example.com\"\")\","),
        "{csv}"
    );
    assert!(csv.contains("\"'@SUM(1)\nrow\""), "{csv}");
    assert!(csv.contains("\n'+cmd,FRONTEND,"), "{csv}");
}

#[test]
fn the_no_script_table_escapes_hostile_names() {
    let mut snapshot = snapshot();
    let pool = &mut snapshot.pools[0];
    pool.name = "<script>alert(1)</script>".into();
    pool.servers[0].name = "\"><img src=x onerror=alert(1)>'".into();
    let html = text(html::render(&snapshot, 1_000, Some(("/haproxy", "rev\"1"))));
    assert!(
        !html.contains("<script>") && !html.contains("<img"),
        "{html}"
    );
    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(html.contains("&quot;&gt;&lt;img src=x onerror=alert(1)&gt;&#39;"));
    assert!(html.contains("name=generation_revision value=\"rev&quot;1\""));
}

#[test]
fn json_keeps_hostile_names_inside_strings() {
    let mut snapshot = snapshot();
    snapshot.pools[0].name = "</script><script>alert(1)</script>\u{2028}".into();
    let bytes = serde_json::to_vec(&snapshot).unwrap();
    let back: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        back["pools"][0]["name"],
        "</script><script>alert(1)</script>\u{2028}"
    );
}
