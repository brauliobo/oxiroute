use crate::TEST_TIMEOUT;
use crate::http_support::HttpResponse as RawResponse;
use crate::support::builders::pool;
use crate::support::builders::proxy_policy_mut;
use crate::support::builders::route;
use crate::support::harness::ProxyHarness;
use crate::support::origin::StatusOrigin;
use oxiroute_config::HttpAffinity;
use oxiroute_config::HttpRoute;
use oxiroute_config::HttpSameSite;
use oxiroute_server::AdministrativeState;
use tokio::time::timeout;

fn affinity_route(affinity: HttpAffinity) -> HttpRoute {
    let mut route = route(None, "/", &[], "farm");
    proxy_policy_mut(&mut route).affinity = Some(affinity);
    route
}

fn cookie_affinity() -> HttpAffinity {
    HttpAffinity::Cookie {
        name: "oxr_srv".into(),
        ttl_s: 600,
        same_site: HttpSameSite::Lax,
        secure: false,
        http_only: true,
    }
}

struct AffinityFarm {
    _origins: [StatusOrigin; 3],
    pub(crate) proxy: ProxyHarness,
}

impl AffinityFarm {
    pub(crate) async fn start(affinity: HttpAffinity, requests: usize) -> Self {
        let origins = [
            StatusOrigin::start(200, "s0").await,
            StatusOrigin::start(200, "s1").await,
            StatusOrigin::start(200, "s2").await,
        ];
        let addresses = origins.each_ref().map(|origin| origin.address);
        let proxy = ProxyHarness::start_with_retries(
            vec![pool("farm", &addresses)],
            vec![affinity_route(affinity)],
            0,
            requests,
        )
        .await;
        Self {
            _origins: origins,
            proxy,
        }
    }

    fn server_name(&self, index: usize) -> String {
        self.proxy.pools[0].health_snapshot().endpoints[index]
            .name
            .clone()
    }

    pub(crate) async fn get(&self, extra_headers: &str) -> RawResponse {
        self.proxy
            .request(&format!(
                "GET /run HTTP/1.1\r\nHost: sticky.test\r\n{extra_headers}"
            ))
            .await
    }
}

fn cookie_pair(response: &RawResponse) -> String {
    response
        .header("set-cookie")
        .expect("affinity cookie")
        .split(';')
        .next()
        .expect("cookie pair")
        .to_owned()
}

#[tokio::test]
async fn cookie_affinity_pins_a_client_to_the_server_that_first_answered() {
    timeout(TEST_TIMEOUT, async {
        let farm = AffinityFarm::start(cookie_affinity(), 11).await;
        let first = farm.get("").await;
        let origin = first.header("x-origin").expect("origin").to_owned();
        let set_cookie = first.header("set-cookie").expect("cookie").to_owned();
        assert!(set_cookie.contains("Max-Age=600") && set_cookie.contains("HttpOnly"));
        assert!(set_cookie.contains("SameSite=Lax") && !set_cookie.contains("Secure"));
        let pair = cookie_pair(&first);

        for _ in 0..9 {
            let response = farm.get(&format!("Cookie: other=1; {pair}\r\n")).await;
            assert_eq!(response.header("x-origin"), Some(origin.as_str()));
            assert_eq!(
                response.header("set-cookie"),
                None,
                "the cookie is already right"
            );
        }

        let stale = farm.get("Cookie: oxr_srv=0000000000000000\r\n").await;
        assert!(
            stale.header("set-cookie").is_some(),
            "a stale id is rewritten"
        );
        farm.proxy.finish().await;
    })
    .await
    .expect("cookie affinity test timed out");
}

#[tokio::test]
async fn cookie_affinity_moves_once_when_the_pinned_server_is_drained() {
    timeout(TEST_TIMEOUT, async {
        let farm = AffinityFarm::start(cookie_affinity(), 5).await;
        let first = farm.get("").await;
        let origin = first.header("x-origin").expect("origin").to_owned();
        let pair = cookie_pair(&first);
        let index = origin[1..].parse::<usize>().expect("origin index");
        farm.proxy.pools[0]
            .set_server_administrative_state(&farm.server_name(index), AdministrativeState::Drain)
            .expect("drain");

        let moved = farm.get(&format!("Cookie: {pair}\r\n")).await;
        let new_origin = moved.header("x-origin").expect("origin").to_owned();
        assert_ne!(new_origin, origin);
        let new_pair = cookie_pair(&moved);
        assert_ne!(new_pair, pair, "the cookie follows the new server");

        for _ in 0..3 {
            let response = farm.get(&format!("Cookie: {new_pair}\r\n")).await;
            assert_eq!(response.header("x-origin"), Some(new_origin.as_str()));
        }
        farm.proxy.finish().await;
    })
    .await
    .expect("cookie drain test timed out");
}

#[tokio::test]
async fn hash_affinity_maps_a_key_to_one_server_and_spreads_keys() {
    timeout(TEST_TIMEOUT, async {
        let farm = AffinityFarm::start(
            HttpAffinity::Hash {
                key: "header:x-session".into(),
            },
            36,
        )
        .await;

        let mut owners = std::collections::BTreeMap::new();
        for round in 0..3 {
            for session in 0..12 {
                let response = farm.get(&format!("X-Session: user-{session}\r\n")).await;
                let origin = response.header("x-origin").expect("origin").to_owned();
                let owner = owners.entry(session).or_insert_with(|| origin.clone());
                assert_eq!(*owner, origin, "session {session} round {round}");
            }
        }
        let distinct = owners
            .values()
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        assert!(distinct >= 2, "keys spread over servers: {owners:?}");
        farm.proxy.finish().await;
    })
    .await
    .expect("hash affinity test timed out");
}

#[tokio::test]
async fn header_affinity_names_the_server_and_falls_back_when_it_cannot_serve() {
    timeout(TEST_TIMEOUT, async {
        let farm = AffinityFarm::start(
            HttpAffinity::Header {
                name: "x-worker".into(),
            },
            5,
        )
        .await;

        let named = format!("X-Worker: {}\r\n", farm.server_name(2));
        for _ in 0..3 {
            let response = farm.get(&named).await;
            assert_eq!(response.header("x-origin"), Some("s2"));
        }
        farm.proxy.pools[0]
            .set_server_administrative_state(&farm.server_name(2), AdministrativeState::Drain)
            .expect("drain");
        let reselected = farm.get(&named).await;
        assert_eq!(reselected.status, 200);
        assert_ne!(reselected.header("x-origin"), Some("s2"));
        let unknown = farm.get("X-Worker: nobody\r\n").await;
        assert_eq!(unknown.status, 200);
        farm.proxy.finish().await;
    })
    .await
    .expect("header affinity test timed out");
}
