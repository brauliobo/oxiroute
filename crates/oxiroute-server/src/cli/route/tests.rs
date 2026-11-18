use clap::Parser as _;
use serde_json::json;

use super::{RouteCommand, target::Request};
use crate::cli::{Cli, Command, view::View};

fn query(target: &str, path: Option<&str>) -> Result<String, String> {
    Request::parse(target, path).map(|request| request.query("GET", None))
}

#[test]
fn a_url_names_its_host_without_the_default_port_and_its_path_without_the_query() {
    assert_eq!(
        query("https://example.com:443/api/users?page=2#top", None).unwrap(),
        "/api/v1/routes/explain?method=GET&path=%2Fapi%2Fusers&host=example.com"
    );
    assert_eq!(
        query("http://alice@example.com:8080", None).unwrap(),
        "/api/v1/routes/explain?method=GET&path=%2F&host=example.com%3A8080"
    );
}

#[test]
fn a_host_and_a_path_or_a_bare_path_name_the_request() {
    assert_eq!(
        query("example.com", Some("/a b")).unwrap(),
        "/api/v1/routes/explain?method=GET&path=%2Fa%20b&host=example.com"
    );
    assert_eq!(
        query("/health", None).unwrap(),
        "/api/v1/routes/explain?method=GET&path=%2Fhealth"
    );
}

#[test]
fn a_request_without_a_host_or_an_absolute_path_is_refused() {
    assert!(query("example.com", None).is_err());
    assert!(query("example.com", Some("health")).is_err());
    assert!(query("https:///path", None).is_err());
}

#[test]
fn the_method_is_upper_cased_and_must_be_a_method() {
    let parse = |method: &str| {
        Cli::try_parse_from(["oxiroute", "route", "test", "/x", "--method", method]).map(|cli| {
            match cli.command {
                Command::Route {
                    command: RouteCommand::Test { method, .. },
                } => method,
                _ => panic!("route command"),
            }
        })
    };
    assert_eq!(parse("post").unwrap(), "POST");
    assert!(parse("not a method").is_err());
}

#[test]
fn a_matching_route_shows_its_action_and_pool() {
    let value = json!({
        "matches": [{
            "listener": "edge", "route": "api", "action": "proxy",
            "pool": { "name": "app", "availableEndpoints": 2, "totalEndpoints": 3 },
            "rescuePool": { "name": "spare", "availableEndpoints": 1, "totalEndpoints": 1 },
        }],
        "unmatched": ["internal"],
    });
    let text = View::RouteTest.document(&value).table();
    assert!(
        text.stdout.contains("api") && text.stdout.contains("2/3 available"),
        "{}",
        text.stdout
    );
    assert!(text.stdout.contains("spare"), "{}", text.stdout);
    assert_eq!(text.notices, ["no route on listeners internal"]);
}
