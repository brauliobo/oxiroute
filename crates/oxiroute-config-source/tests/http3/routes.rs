use oxiroute_config::ConfigDraft;
use serde_json::json;

fn http3_config(request_buffering: bool, tls_profile: Option<&str>) -> ConfigDraft {
    serde_json::from_value(json!({
        "version": 1,
        "certificates": [{
            "name": "local",
            "dns_names": ["localhost"],
            "source": {"type": "self_signed_development", "validity_days": 7}
        }],
        "tls_profiles": [{
            "name": "h3",
            "certificates": ["local"],
            "default_certificate": "local",
            "min_version": "1.3",
            "alpn": ["h3"]
        }],
        "listeners": [{
            "name": "h3",
            "bind": {"type": "udp", "address": "127.0.0.1:9443"},
            "protocol": "http3",
            "service": "web",
            "tls_profile": tls_profile
        }],
        "http_services": [{
            "name": "web",
            "routes": [{
                "path": {"kind": "exact", "value": "/"},
                "policy": {"request_buffering": request_buffering},
                "action": {"type": "fixed_response", "status": 200, "body": "ok"}
            }]
        }]
    }))
    .expect("HTTP/3 test configuration")
}

fn reverse_http3_proxy_config(min: &str, max: &str) -> ConfigDraft {
    let mut config = http3_config(true, Some("h3"));
    config.upstream_pools.push(
        serde_json::from_value(json!({
            "name": "web",
            "endpoints": [{"type": "socket", "address": "127.0.0.1:3000"}],
            "tls": {"server_name": "localhost"},
            "http_versions": {"min": min, "max": max},
        }))
        .expect("versioned upstream pool"),
    );
    config.http_services[0].routes[0].action = serde_json::from_value(json!({
        "type": "proxy",
        "upstream_pool": "web",
        "policy": {},
    }))
    .expect("proxy HTTP/3 route");
    config
}

fn add_tcp_listener(config: &mut ConfigDraft) {
    config.listeners.push(
        serde_json::from_value(json!({
            "name": "tcp",
            "bind": {"type": "socket", "address": "127.0.0.1:9080"},
            "protocol": "http",
            "service": "web",
        }))
        .expect("TCP listener sharing the service"),
    );
}

#[test]
fn accepts_a_bounded_reverse_http3_listener() {
    http3_config(true, Some("h3"))
        .validate()
        .expect("valid reverse HTTP/3 config");
}

#[test]
fn accepts_bounded_reverse_http3_static_files() {
    let mut config = http3_config(true, Some("h3"));
    config.http_services[0].routes[0].action = serde_json::from_value(json!({
        "type": "static_files",
        "root_directory": "/srv/www",
    }))
    .expect("static HTTP/3 route");

    config
        .validate()
        .expect("static files are an active HTTP/3 response");
}

#[test]
fn rejects_reverse_http3_request_body_limits_above_the_runtime_bound() {
    let mut config = http3_config(true, Some("h3"));
    config.http_services[0].max_request_body_bytes = Some(64 * 1024 * 1024 + 1);
    let error = config
        .validate()
        .expect_err("oversized H3 service body limit");
    assert!(
        error
            .to_string()
            .contains("HTTP/3 service request body exceeds the 64 MiB limit")
    );

    let mut config = http3_config(true, Some("h3"));
    config.http_services[0].routes[0]
        .policy
        .max_request_body_bytes = Some(64 * 1024 * 1024 + 1);
    let error = config
        .validate()
        .expect_err("oversized H3 route body limit");
    assert!(
        error
            .to_string()
            .contains("HTTP/3 route request body exceeds the 64 MiB limit")
    );
}

#[test]
fn accepts_reverse_http3_routes_with_h1_and_h2_pools() {
    for (min, max) in [("1.1", "1.1"), ("1.1", "2"), ("2", "2")] {
        reverse_http3_proxy_config(min, max)
            .validate()
            .unwrap_or_else(|error| panic!("reverse H3 accepts a {min}-{max} pool: {error}"));
    }
}

#[test]
fn accepts_reverse_http3_routes_with_an_exact_h3_pool() {
    reverse_http3_proxy_config("3", "3")
        .validate()
        .expect("reverse H3 accepts an exact H3 upstream pool");
}

#[test]
fn rejects_reverse_http3_routes_with_an_unresolvable_pool() {
    let mut config = reverse_http3_proxy_config("1.1", "1.1");
    config.upstream_pools.clear();
    config
        .validate()
        .expect_err("a proxy route must name a configured pool");
}

#[test]
fn shares_a_service_with_h1_and_h2_pools_between_http3_and_tcp_listeners() {
    for (min, max) in [("1.1", "1.1"), ("1.1", "2"), ("2", "2")] {
        let mut config = reverse_http3_proxy_config(min, max);
        add_tcp_listener(&mut config);
        config
            .validate()
            .unwrap_or_else(|error| panic!("a {min}-{max} pool is shared: {error}"));
    }
}

#[test]
fn rejects_sharing_a_service_with_an_h3_pool_with_a_tcp_listener() {
    let mut config = reverse_http3_proxy_config("3", "3");
    add_tcp_listener(&mut config);
    let error = config
        .validate()
        .expect_err("an H3 pool cannot serve a TCP listener");
    assert!(
        error
            .to_string()
            .contains("a service using an HTTP/3 upstream pool cannot be shared"),
        "unexpected rejection: {error}"
    );
}

#[test]
fn rejects_reverse_http3_routes_with_policies_its_proxy_path_does_not_apply() {
    for policy in [
        json!({"retry": {"budget": {"per_second": 5}}}),
        json!({"rescue": {"target": "pool", "pool": "other", "statuses": [502]}}),
        json!({"affinity": {"type": "header", "name": "x-server"}}),
        json!({"queue_class": "gold"}),
        json!({"queue_headers": true}),
    ] {
        let mut config = reverse_http3_proxy_config("1.1", "1.1");
        config.http_services[0].routes[0].action = serde_json::from_value(json!({
            "type": "proxy",
            "upstream_pool": "web",
            "policy": policy,
        }))
        .expect("proxy HTTP/3 route");
        let error = config
            .validate()
            .expect_err("a policy the HTTP/3 proxy path ignores");
        assert!(
            error
                .to_string()
                .contains("HTTP/3 reverse routes do not support"),
            "unexpected rejection for {policy}: {error}"
        );
    }
}

#[test]
fn rejects_reverse_http3_without_request_buffering() {
    let error = http3_config(false, Some("h3"))
        .validate()
        .expect_err("unbuffered reverse HTTP/3 request");
    assert!(
        error
            .to_string()
            .contains("HTTP/3 requires request buffering")
    );
}

#[test]
fn rejects_reverse_http3_without_tls_profile() {
    let error = http3_config(true, None)
        .validate()
        .expect_err("TLS-less reverse HTTP/3 listener");
    assert!(
        error
            .to_string()
            .contains("HTTP/3 requires a TLS 1.3 profile advertising only h3")
    );
}
