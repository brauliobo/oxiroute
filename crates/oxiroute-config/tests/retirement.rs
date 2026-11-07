use oxiroute_config::{ConfigDraft, ConfigError, ValidatedConfig};
use serde_json::{Value, json};

const THREE_HOURS_MS: u64 = 10_800_000;
const ONE_DAY_MS: u64 = 86_400_000;

fn service(name: &str, extra: Value) -> Value {
    let mut service = json!({
        "name": name,
        "routes": [{
            "path": {"kind": "exact", "value": "/"},
            "action": {"type": "fixed_response", "status": 200, "body": "ok"},
        }],
    });
    service
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    service
}

fn draft(extra: Value) -> ConfigDraft {
    let mut config = json!({
        "version": 1,
        "listeners": [],
        "http_services": [service("web", json!({}))],
    });
    config
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value(config).expect("authored configuration")
}

fn validate(extra: Value) -> Result<ValidatedConfig, ConfigError> {
    draft(extra).validate()
}

fn retirement(drain_deadline_ms: u64, max_retired_generations: u64) -> Value {
    json!({"retirement": {
        "drain_deadline_ms": drain_deadline_ms,
        "max_retired_generations": max_retired_generations,
    }})
}

#[test]
fn an_absent_retirement_block_keeps_the_three_hour_cap_one_default() {
    let config = validate(json!({})).unwrap();

    assert_eq!(config.as_draft().retirement, None);
    assert_eq!(config.retired_generation_limit(), 4);
    assert_eq!(config.retirement_deadline_ms(), THREE_HOURS_MS);
    let canonical = serde_json::to_value(&config).unwrap();
    assert!(canonical.get("retirement").is_none(), "{canonical}");
}

#[test]
fn drain_deadline_and_cap_accept_their_bounds() {
    for (deadline, cap) in [(1_000, 1), (ONE_DAY_MS, 16), (THREE_HOURS_MS, 4)] {
        let config = validate(retirement(deadline, cap)).expect("bounds are valid");
        assert_eq!(config.retirement_deadline_ms(), deadline);
        assert_eq!(
            config.retired_generation_limit(),
            usize::try_from(cap).unwrap()
        );
    }
}

#[test]
fn drain_deadline_and_cap_reject_values_outside_their_bounds() {
    for (deadline, cap, pointer) in [
        (999, 4, "/retirement/drain_deadline_ms"),
        (0, 4, "/retirement/drain_deadline_ms"),
        (ONE_DAY_MS + 1, 4, "/retirement/drain_deadline_ms"),
        (THREE_HOURS_MS, 0, "/retirement/max_retired_generations"),
        (THREE_HOURS_MS, 17, "/retirement/max_retired_generations"),
    ] {
        let error = validate(retirement(deadline, cap)).expect_err("out of bounds");
        assert_eq!(error.pointer().as_deref(), Some(pointer), "{error}");
    }
}

#[test]
fn an_unset_deadline_follows_the_longest_http_request_deadline() {
    let long = 4 * 3_600_000;
    let config = validate(json!({"http_services": [
        service("web", json!({})),
        service("render", json!({"upstream_io_timeout_ms": long})),
    ]}))
    .unwrap();
    assert_eq!(config.retirement_deadline_ms(), long);

    let config = validate(json!({"retirement": {}, "http_services": [
        service("render", json!({"upstream_io_timeout_ms": ONE_DAY_MS})),
    ]}))
    .unwrap();
    assert_eq!(config.retirement_deadline_ms(), ONE_DAY_MS);
}

#[test]
fn an_explicit_deadline_wins_over_the_derived_default() {
    let config = validate(json!({
        "retirement": {"drain_deadline_ms": 5_000},
        "http_services": [service("render", json!({"upstream_io_timeout_ms": 7_200_000}))],
    }))
    .unwrap();
    assert_eq!(config.retirement_deadline_ms(), 5_000);
}

#[test]
fn a_service_deadline_only_raises_the_ceiling() {
    let services = |render_ms: u64| {
        json!({
            "retirement": {"drain_deadline_ms": 60_000},
            "http_services": [
                service("web", json!({"drain_deadline_ms": 30_000})),
                service("render", json!({"drain_deadline_ms": render_ms})),
            ],
        })
    };
    assert_eq!(
        validate(services(7_200_000))
            .unwrap()
            .retirement_deadline_ms(),
        7_200_000
    );
    assert_eq!(
        validate(services(45_000)).unwrap().retirement_deadline_ms(),
        60_000
    );
}

#[test]
fn service_deadlines_exist_on_http_forward_proxy_and_l4_services_only() {
    for (key, service) in [
        (
            "http_services",
            service("a", json!({"drain_deadline_ms": 999})),
        ),
        (
            "forward_proxy_services",
            json!({"name": "a", "drain_deadline_ms": 999}),
        ),
        (
            "l4_services",
            json!({"name": "a", "upstream_pool": "p", "drain_deadline_ms": 999}),
        ),
    ] {
        let error = serde_json::from_value::<ConfigDraft>(json!({
            "version": 1, "listeners": [], key: [service],
        }))
        .map(ConfigDraft::validate);
        match error {
            Ok(Err(error)) => {
                assert_eq!(
                    error.pointer().as_deref(),
                    Some(format!("/{key}/a/drain_deadline_ms").as_str())
                );
            }
            other => panic!("{key}: {other:?}"),
        }
    }
    let unknown = serde_json::from_value::<ConfigDraft>(json!({
        "version": 1, "listeners": [], "upstream_pools": [{"name": "p", "servers": [], "drain_deadline_ms": 5_000}],
    }));
    assert!(unknown.is_err());
}

#[test]
fn the_retirement_block_survives_a_validate_round_trip() {
    let config = validate(retirement(90_000, 2)).unwrap();
    let again = config.to_draft().validate().unwrap();
    assert_eq!(again, config);
    assert_eq!(
        serde_json::to_value(&config).unwrap()["retirement"],
        json!({"drain_deadline_ms": 90_000, "max_retired_generations": 2})
    );
}
