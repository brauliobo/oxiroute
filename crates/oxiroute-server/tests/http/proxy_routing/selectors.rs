use crate::TEST_TIMEOUT;
use crate::support::builders::pool;
use crate::support::builders::proxy_policy_mut;
use crate::support::builders::route;
use crate::support::harness::ProxyHarness;
use crate::support::origin::StatusOrigin;
use oxiroute_config::HttpPoolSelect;
use oxiroute_config::HttpPoolSelectRule;
use oxiroute_config::HttpRoute;
use oxiroute_config::HttpSelectInvalid;
use oxiroute_config::HttpSelectSpill;
use oxiroute_server::AdministrativeState;
use tokio::time::timeout;

fn select_route(on_invalid: HttpSelectInvalid, forward: bool, spill: HttpSelectSpill) -> HttpRoute {
    let mut route = route(None, "/", &[], "any");
    proxy_policy_mut(&mut route).select = Some(HttpPoolSelect {
        header: "x-min-vram".into(),
        rules: vec![
            HttpPoolSelectRule {
                at_most: 12,
                pool: "small".into(),
            },
            HttpPoolSelectRule {
                at_most: 24,
                pool: "large".into(),
            },
        ],
        on_invalid,
        forward,
        spill,
    });
    route
}

struct SelectFarm {
    pub(crate) origins: [StatusOrigin; 3],
    pub(crate) proxy: ProxyHarness,
}

impl SelectFarm {
    pub(crate) async fn start(route: HttpRoute, requests: usize) -> Self {
        let origins = [
            StatusOrigin::start(200, "small").await,
            StatusOrigin::start(200, "large").await,
            StatusOrigin::start(200, "any").await,
        ];
        let proxy = ProxyHarness::start_with_retries(
            vec![
                pool("small", &[origins[0].address]),
                pool("large", &[origins[1].address]),
                pool("any", &[origins[2].address]),
            ],
            vec![route],
            0,
            requests,
        )
        .await;
        Self { origins, proxy }
    }

    async fn served_by(&self, header: Option<&str>) -> String {
        let extra = header.map_or_else(String::new, |value| format!("X-Min-Vram: {value}\r\n"));
        let response = self
            .proxy
            .request(&format!(
                "GET /run HTTP/1.1\r\nHost: select.test\r\n{extra}"
            ))
            .await;
        if response.status == 200 {
            response
                .text()
                .rsplit('\n')
                .next()
                .unwrap_or_default()
                .to_owned()
        } else {
            response.status.to_string()
        }
    }
}

#[tokio::test]
async fn header_selector_sends_each_capability_to_its_pool() {
    timeout(TEST_TIMEOUT, async {
        let farm = SelectFarm::start(
            select_route(HttpSelectInvalid::Default, false, HttpSelectSpill::None),
            6,
        )
        .await;

        let mut served = Vec::new();
        for header in [
            None,
            Some("8"),
            Some("12"),
            Some("12.5"),
            Some("24"),
            Some("100"),
        ] {
            served.push(farm.served_by(header).await);
        }

        assert_eq!(served, ["any", "small", "small", "large", "large", "any"]);
        let selections = farm
            .proxy
            .metrics
            .snapshot()
            .expect("metrics")
            .process
            .pool_selections;
        let counts = selections
            .iter()
            .map(|selection| (selection.pool.as_str(), selection.count))
            .collect::<Vec<_>>();
        assert_eq!(counts, [("any", 2), ("large", 2), ("small", 2)]);
        farm.proxy.finish().await;
    })
    .await
    .expect("selector test timed out");
}

#[tokio::test]
async fn header_selector_hides_the_header_from_the_upstream_unless_forwarded() {
    timeout(TEST_TIMEOUT, async {
        for forward in [false, true] {
            let farm = SelectFarm::start(
                select_route(HttpSelectInvalid::Default, forward, HttpSelectSpill::None),
                1,
            )
            .await;

            farm.served_by(Some("8")).await;

            let heads = farm.origins[0].heads();
            assert_eq!(heads.len(), 1);
            assert_eq!(heads[0].contains("x-min-vram"), forward, "{}", heads[0]);
            farm.proxy.finish().await;
        }
    })
    .await
    .expect("selector header test timed out");
}

#[tokio::test]
async fn header_selector_invalid_values_follow_on_invalid() {
    timeout(TEST_TIMEOUT, async {
        let rejecting = SelectFarm::start(
            select_route(HttpSelectInvalid::Reject, false, HttpSelectSpill::None),
            3,
        )
        .await;
        let mut served = Vec::new();
        for header in ["abc", "-1", "NaN"] {
            served.push(rejecting.served_by(Some(header)).await);
        }
        assert_eq!(served, ["400", "400", "400"]);
        assert!(rejecting.origins.iter().all(|origin| origin.hits() == 0));
        rejecting.proxy.finish().await;

        let lenient = SelectFarm::start(
            select_route(HttpSelectInvalid::Default, false, HttpSelectSpill::None),
            1,
        )
        .await;
        assert_eq!(lenient.served_by(Some("abc")).await, "any");
        lenient.proxy.finish().await;
    })
    .await
    .expect("selector invalid test timed out");
}

#[tokio::test]
async fn header_selector_spills_up_only_when_the_pool_has_no_selectable_server() {
    timeout(TEST_TIMEOUT, async {
        let spilling = SelectFarm::start(
            select_route(HttpSelectInvalid::Default, false, HttpSelectSpill::Up),
            3,
        )
        .await;
        assert_eq!(spilling.served_by(Some("8")).await, "small");
        spilling.proxy.pools[0].set_administrative_state(AdministrativeState::Drain);
        assert_eq!(spilling.served_by(Some("8")).await, "large");
        spilling.proxy.pools[1].set_administrative_state(AdministrativeState::Drain);
        assert_eq!(spilling.served_by(Some("8")).await, "any");
        spilling.proxy.finish().await;

        let strict = SelectFarm::start(
            select_route(HttpSelectInvalid::Default, false, HttpSelectSpill::None),
            1,
        )
        .await;
        strict.proxy.pools[0].set_administrative_state(AdministrativeState::Drain);
        assert_eq!(strict.served_by(Some("8")).await, "503");
        strict.proxy.finish().await;
    })
    .await
    .expect("selector spill test timed out");
}
