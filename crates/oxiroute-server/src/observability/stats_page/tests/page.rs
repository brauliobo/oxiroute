use oxiroute_config::StatsPageAdminPolicy::{Disabled, Localhost};
use serde_json::Value;

use super::fixture::{API, WEB, fixture, page, page_config};
use crate::{
    AdministrativeState,
    observability::stats_page::{
        HaproxyStatsPage, admin_form::StatsPageAdminTarget, page::secure_page_response,
    },
};

async fn json(page: &HaproxyStatsPage, peer_is_loopback: bool) -> Value {
    let response = page
        .handle("GET", "/haproxy/snapshot.json", peer_is_loopback)
        .await;
    assert_eq!(response.status, 200);
    assert_eq!(response.content_type, "application/json");
    serde_json::from_slice(&response.body).expect("snapshot JSON")
}

#[tokio::test]
async fn only_the_page_the_snapshot_and_the_csv_answer_under_the_prefix() {
    let fixture = fixture();
    let page = page(&fixture, Localhost);
    for (path, content_type) in [
        ("/haproxy", "text/html; charset=utf-8"),
        ("/haproxy/", "text/html; charset=utf-8"),
        ("/haproxy/snapshot.json", "application/json"),
        ("/haproxy/stats.csv", "text/csv; charset=utf-8"),
    ] {
        let response = page.handle("GET", path, false).await;
        assert_eq!(
            (response.status, response.content_type),
            (200, content_type),
            "{path}"
        );
    }
    for path in [
        "/haproxy/servers",
        "/haproxy/snapshot.json/",
        "/haproxy/stats.csv.bak",
        "/haproxyfoo",
        "/haprox",
        "/",
        "/metrics",
        "/ready",
        "/status.json",
        "/api/v1/status",
        "/stats",
    ] {
        assert_eq!(page.handle("GET", path, true).await.status, 404, "{path}");
    }
    assert_eq!(page.handle("PUT", "/haproxy", true).await.status, 405);
    assert_eq!(
        page.handle("POST", "/haproxy/snapshot.json", true)
            .await
            .status,
        405
    );
    assert_eq!(
        page.handle("POST", "/haproxy/stats.csv", true).await.status,
        405
    );
    assert_eq!(page.handle("POST", "/haproxy", true).await.status, 400);
    assert_eq!(
        page.handle("POST", "/haproxy/nothing", true).await.status,
        404
    );
    for path in ["/haproxy", "/haproxy/snapshot.json", "/haproxy/stats.csv"] {
        let head = page.handle("HEAD", path, true).await;
        assert!((head.status, head.body.is_empty()) == (200, true), "{path}");
    }
}

#[tokio::test]
async fn a_prefix_ending_in_a_slash_serves_the_same_routes() {
    let fixture = fixture();
    let mut config = page_config(10_000, Disabled);
    config.uri_prefix = "/".into();
    let page = HaproxyStatsPage::new(&config, fixture.generations.clone());
    for path in ["/", "/snapshot.json", "/stats.csv"] {
        assert_eq!(page.handle("GET", path, false).await.status, 200, "{path}");
    }
    assert_eq!(page.handle("GET", "/other", false).await.status, 404);
}

#[tokio::test]
async fn every_peer_sees_the_same_snapshot_and_only_the_admin_flag_differs() {
    let fixture = fixture();
    let page = page(&fixture, Localhost);
    let mut remote = json(&page, false).await;
    let mut local = json(&page, true).await;
    assert_eq!(
        (&remote["admin"], &local["admin"]),
        (&Value::Bool(false), &Value::Bool(true))
    );
    remote.as_object_mut().unwrap().remove("admin");
    local.as_object_mut().unwrap().remove("admin");
    assert_eq!(remote, local);
    // Addresses, binds and the revision are not withheld from a remote peer.
    let text = remote.to_string();
    assert!(text.contains("192.0.2.10:3000"));
    assert!(text.contains(&fixture.revision));
    let front = remote["listeners"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["name"] == "front")
        .unwrap();
    assert!(
        front["bind"]
            .as_str()
            .unwrap()
            .starts_with("socket:127.0.0.1:")
    );
}

#[tokio::test]
async fn administration_is_offered_only_to_loopback_peers_of_a_page_that_enables_it() {
    let fixture = fixture();
    let disabled = page(&fixture, Disabled);
    assert_eq!(json(&disabled, true).await["admin"], false);
    let enabled = page(&fixture, Localhost);
    assert_eq!(json(&enabled, false).await["admin"], false);
    assert_eq!(json(&enabled, true).await["admin"], true);
    assert!(!html(&enabled, false).await.contains("<form"));
    let local = html(&enabled, true).await;
    assert!(local.contains("<form method=post action=\"/haproxy\""));
    assert!(local.contains(&format!(
        "name=generation_revision value=\"{}\"",
        fixture.revision
    )));
}

async fn html(page: &HaproxyStatsPage, peer_is_loopback: bool) -> String {
    String::from_utf8(page.handle("GET", "/haproxy", peer_is_loopback).await.body).expect("HTML")
}

#[tokio::test]
async fn the_no_script_table_lists_every_server_and_refreshes() {
    let fixture = fixture();
    let page = HaproxyStatsPage::new(&page_config(2_500, Disabled), fixture.generations.clone());
    let html = String::from_utf8(page.handle("GET", "/haproxy", false).await.body).unwrap();
    assert!(html.contains("name=\"oxiroute-refresh-ms\" content=\"2500\""));
    assert!(html.contains("http-equiv=\"refresh\" content=\"2.500\""));
    for server in ["origin-a", "origin-b", "origin-c"] {
        assert!(html.contains(&format!("<td>{server}</td><td>")), "{server}");
    }
    assert!(html.contains(&format!(
        "<td>{WEB}</td><td>origin-a</td><td>ready</td><td>unchecked</td><td>0</td>"
    )));
    assert!(html.contains(&format!("<td>{API}</td>")));
}

#[tokio::test]
async fn an_administrative_change_is_visible_to_the_next_read() {
    let fixture = fixture();
    let page = page(&fixture, Localhost);
    let before = json(&page, true).await;
    assert_eq!(
        before["pools"][0]["servers"][0]["administrativeState"],
        "ready"
    );
    let mut target = StatsPageAdminTarget {
        generation_revision: fixture.revision.clone(),
        pool: WEB.into(),
        server: "origin-a".into(),
        state: "drain".into(),
    };
    assert_eq!(page.admin_response(&target, false, true).status, 403);
    assert_eq!(page.admin_response(&target, true, false).status, 403);
    assert_eq!(page.admin_response(&target, true, true).status, 204);
    let after = json(&page, true).await;
    assert_eq!(
        after["pools"][0]["servers"][0]["administrativeState"],
        "drain"
    );
    assert_eq!(after["pools"][0]["servers"][0]["status"], "drain");
    for (state, expected) in [
        ("maintenance", AdministrativeState::Maintenance),
        ("ready", AdministrativeState::Ready),
    ] {
        target.state = state.into();
        assert_eq!(page.admin_response(&target, true, true).status, 204);
        assert_eq!(
            fixture.pool(WEB).health_snapshot().endpoints[0].administrative_state,
            expected
        );
    }
    target.generation_revision = "stale".into();
    let response = page.admin_response(&target, true, true);
    assert_eq!(response.status, 409);
    let body: Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(body["error"]["code"], "generation_conflict");
    target.generation_revision = fixture.revision.clone();
    target.state = "bogus".into();
    assert_eq!(page.admin_response(&target, true, true).status, 400);
    assert_eq!(
        page.admin_response(
            &StatsPageAdminTarget {
                state: "ready".into(),
                pool: "nope".into(),
                ..target
            },
            true,
            true
        )
        .status,
        404
    );
}

#[tokio::test]
async fn a_page_without_an_active_generation_reports_the_failure() {
    let page = HaproxyStatsPage::new(
        &page_config(1_000, Disabled),
        crate::GenerationManager::new(),
    );
    for path in ["/haproxy", "/haproxy/snapshot.json", "/haproxy/stats.csv"] {
        let response = page.handle("GET", path, true).await;
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(
            (response.status, &body["error"]["code"]),
            (503, &Value::from("stats_unavailable")),
            "{path}"
        );
    }
}

#[tokio::test]
async fn responses_are_never_cached_and_carry_the_strict_policy() {
    let fixture = fixture();
    let page = page(&fixture, Disabled);
    let secured = secure_page_response(page.handle("GET", "/haproxy/snapshot.json", false).await);
    let headers = secured.headers();
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(headers["x-frame-options"], "DENY");
    assert_eq!(headers["referrer-policy"], "no-referrer");
    assert_eq!(
        headers["content-security-policy"],
        "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; \
         font-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'"
    );
}
