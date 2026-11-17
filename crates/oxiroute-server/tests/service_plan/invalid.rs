use oxiroute_config::{ConfigError, HealthCheck, HealthCheckType, HttpRouteAction};
use oxiroute_server::ServicePlanError;

use crate::canonical::canonical_config;
use crate::config_support::{loopback_endpoint as socket_endpoint, service_specs};

#[test]
fn rejects_an_invalid_programmatic_listener_without_panicking() {
    let mut config = canonical_config();
    config.listeners[0].service = None;

    assert!(matches!(
        config.validate(),
        Err(ConfigError::MissingListenerService { listener, .. }) if listener == "web"
    ));
}

#[test]
fn rejects_an_invalid_programmatic_route_pool_reference() {
    let mut config = canonical_config();
    let HttpRouteAction::Proxy { upstream_pool, .. } =
        &mut config.http_services[0].routes[0].action
    else {
        panic!("test route must proxy");
    };
    *upstream_pool = "missing".into();

    assert!(matches!(
        config.validate(),
        Err(ConfigError::UnknownRouteUpstreamPool { service, route: 0, pool })
            if service == "api" && pool == "missing"
    ));
}

#[test]
fn refuses_to_discard_a_required_health_supervisor() {
    let mut config = canonical_config();
    config.upstream_pools[0].health_check = Some(HealthCheck {
        kind: HealthCheckType::Tcp,
        interval_ms: 1_000,
        timeout_ms: 100,
        healthy_threshold: 1,
        unhealthy_threshold: 1,
        startup: oxiroute_config::HealthStartup::default(),
        fast_interval_ms: None,
        down_interval_ms: None,
        host: None,
        path: None,
        expected_status: None,
        http_version: None,
    });

    assert!(matches!(
        service_specs(&config.clone().validate().unwrap()),
        Err(ServicePlanError::HealthSupervisorRequired)
    ));
}

#[test]
fn rejects_invalid_programmatic_pool_definitions() {
    let mut duplicate_name = canonical_config();
    duplicate_name.upstream_pools[1].name = "api".into();
    assert!(matches!(
        duplicate_name.validate(),
        Err(ConfigError::DuplicateName { namespace: "upstream pool", name }) if name == "api"
    ));

    let mut duplicate_endpoint = canonical_config();
    duplicate_endpoint.upstream_pools[0].endpoints[1] = socket_endpoint(3000);
    assert!(matches!(
        duplicate_endpoint.validate(),
        Err(ConfigError::DuplicateUpstreamEndpoint { pool, .. }) if pool == "api"
    ));

    let mut zero_port = canonical_config();
    zero_port.upstream_pools[0].endpoints[0] = socket_endpoint(0);
    assert!(matches!(
        zero_port.validate(),
        Err(ConfigError::ZeroPort { kind: "upstream pool", name, field: "endpoints" })
            if name == "api"
    ));
}
