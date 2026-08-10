use oxiroute_config::{
    Config, ForwardAccessMatcher, HttpRouteAction, load_lua, render_lua, validate_config,
};
use serde_json::json;

fn authored_config() -> Config {
    serde_json::from_value(json!({
        "version": 1,
        "listeners": [],
        "cache_stores": [{"name": "memory", "type": "memory"}],
        "upstream_pools": [{
            "name": "origin",
            "endpoints": [{"type": "socket", "address": "127.0.0.1:3000"}]
        }],
        "http_services": [{
            "name": "web",
            "routes": [{
                "path": {"kind": "exact", "value": "/"},
                "methods": ["x1", "get", "foo!", "M-SEARCH"],
                "action": {
                    "type": "proxy",
                    "upstream_pool": "origin",
                    "policy": {"cache": {"store": "memory", "methods": ["head", "get"]}}
                }
            }]
        }],
        "forward_proxy_services": [{
            "name": "egress",
            "access_policy": {
                "rules": [{
                    "action": "allow",
                    "conditions": [{
                        "type": "methods",
                        "methods": ["m-search", "x1", "foo!", "get"]
                    }]
                }]
            }
        }]
    }))
    .expect("authored HTTP-method configuration")
}

fn route_methods(config: &Config) -> &[String] {
    &config.http_services[0].routes[0].methods
}

fn cache_methods(config: &Config) -> &[String] {
    let HttpRouteAction::Proxy { policy, .. } = &config.http_services[0].routes[0].action else {
        panic!("proxy route");
    };
    &policy.cache.as_ref().expect("cache policy").methods
}

fn forward_methods(config: &Config) -> &[String] {
    let ForwardAccessMatcher::Methods { methods } = &config.forward_proxy_services[0]
        .access_policy
        .as_ref()
        .expect("access policy")
        .rules[0]
        .conditions[0]
        .matcher
    else {
        panic!("method matcher");
    };
    methods
}

#[test]
fn serde_preserves_authored_methods_and_validation_canonicalizes_each_owner() {
    let authored = authored_config();
    assert_eq!(route_methods(&authored), ["x1", "get", "foo!", "M-SEARCH"]);
    assert_eq!(cache_methods(&authored), ["head", "get"]);
    assert_eq!(
        forward_methods(&authored),
        ["m-search", "x1", "foo!", "get"]
    );

    let mut canonical = authored.clone();
    validate_config(&mut canonical).expect("valid HTTP tokens");
    assert_eq!(route_methods(&canonical), ["FOO!", "GET", "M-SEARCH", "X1"]);
    assert_eq!(cache_methods(&canonical), ["GET", "HEAD"]);
    assert_eq!(
        forward_methods(&canonical),
        ["M-SEARCH", "X1", "FOO!", "GET"]
    );
}

#[test]
fn render_validates_a_clone_without_mutating_authored_methods() {
    let authored = authored_config();
    let snapshot = authored.clone();

    let rendered = render_lua(&authored).expect("render authored methods");
    let rendered = load_lua(&rendered).expect("reload rendered methods");

    assert_eq!(authored, snapshot);
    assert_eq!(route_methods(&rendered), ["FOO!", "GET", "M-SEARCH", "X1"]);
    assert_eq!(cache_methods(&rendered), ["GET", "HEAD"]);
    assert_eq!(
        forward_methods(&rendered),
        ["M-SEARCH", "X1", "FOO!", "GET"]
    );
}

#[test]
fn rejects_case_duplicates_after_normalization_at_each_owner() {
    for mutate in [
        |config: &mut Config| {
            config.http_services[0].routes[0].methods = vec!["GET".into(), "get".into()];
        },
        |config: &mut Config| {
            let HttpRouteAction::Proxy { policy, .. } =
                &mut config.http_services[0].routes[0].action
            else {
                panic!("proxy route");
            };
            policy.cache.as_mut().expect("cache policy").methods = vec!["GET".into(), "get".into()];
        },
        |config: &mut Config| {
            let ForwardAccessMatcher::Methods { methods } = &mut config.forward_proxy_services[0]
                .access_policy
                .as_mut()
                .expect("access policy")
                .rules[0]
                .conditions[0]
                .matcher
            else {
                panic!("method matcher");
            };
            *methods = vec!["GET".into(), "get".into()];
        },
    ] {
        let mut config = authored_config();
        mutate(&mut config);
        assert!(validate_config(&mut config).is_err());
    }
}

#[test]
fn each_owner_rejects_invalid_http_tokens() {
    for invalid in [
        "",
        "FOO@",
        "GÉT",
        "GET\u{1}",
        "X12345678901234567890123456789012",
    ] {
        let mut route = authored_config();
        route.http_services[0].routes[0].methods = vec![invalid.into()];
        assert!(
            validate_config(&mut route).is_err(),
            "route accepted {invalid:?}"
        );

        let mut cache = authored_config();
        let HttpRouteAction::Proxy { policy, .. } = &mut cache.http_services[0].routes[0].action
        else {
            panic!("proxy route");
        };
        policy.cache.as_mut().expect("cache policy").methods = vec![invalid.into()];
        assert!(
            validate_config(&mut cache).is_err(),
            "cache accepted {invalid:?}"
        );

        let mut forward = authored_config();
        let ForwardAccessMatcher::Methods { methods } = &mut forward.forward_proxy_services[0]
            .access_policy
            .as_mut()
            .expect("access policy")
            .rules[0]
            .conditions[0]
            .matcher
        else {
            panic!("method matcher");
        };
        *methods = vec![invalid.into()];
        assert!(
            validate_config(&mut forward).is_err(),
            "forward accepted {invalid:?}"
        );
    }
}

#[test]
fn cache_keeps_its_get_head_restriction_after_token_normalization() {
    for method in ["m-search", "x1", "foo!"] {
        let mut config = authored_config();
        let HttpRouteAction::Proxy { policy, .. } = &mut config.http_services[0].routes[0].action
        else {
            panic!("proxy route");
        };
        policy.cache.as_mut().expect("cache policy").methods = vec![method.into()];
        assert!(
            validate_config(&mut config).is_err(),
            "cache accepted {method}"
        );
    }
}
