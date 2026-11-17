use oxiroute_config::{ConfigError, StatsPageAdminPolicy};

use crate::lua_support::{load_lua, render_lua};

#[test]
fn loads_multiple_ipv4_ipv6_statistics_binds_and_admin_token_path() {
    let config = load_lua(
        r#"return {
          version = 1,
          listeners = {},
          stats = {
            binds = { "127.0.0.1:8404", "[::1]:8404" },
            admin_token_file = "/etc/oxiroute/stats.token",
          },
        }"#,
    )
    .expect("statistics config");
    let stats = config.stats.expect("statistics");
    assert_eq!(stats.binds.len(), 2);
    assert_eq!(
        stats.admin_token_file.as_deref(),
        Some(std::path::Path::new("/etc/oxiroute/stats.token"))
    );
}

#[test]
fn rejects_statistics_binds_that_overlap_management() {
    let error = load_lua(
        r#"return {
          version = 1,
          management = { bind = "127.0.0.1:8404" },
          stats = { binds = { "0.0.0.0:8404" } },
          listeners = {},
        }"#,
    )
    .expect_err("overlap");
    assert!(matches!(error, ConfigError::OverlappingBind { .. }));
}

#[test]
fn page_only_statistics_validate_render_and_roundtrip() {
    let config = load_lua(
        r#"return {
          version = 1,
          listeners = {},
          stats = {
            pages = {
              {
                bind = "127.0.0.1:8404",
                uri_prefix = "/stats",
                refresh_ms = 10000,
                admin = "localhost",
                max_connections = 250,
                downstream_timeouts = {
                  client_timeout_ms = 600000,
                  request_timeout_ms = 600000,
                  keepalive_timeout_ms = 60000,
                },
              },
            },
          },
        }"#,
    )
    .expect("page-only statistics config");
    let page = &config.stats.as_ref().expect("statistics").pages[0];
    assert!(config.stats.as_ref().expect("statistics").binds.is_empty());
    assert_eq!(page.uri_prefix, "/stats");
    assert_eq!(page.refresh_ms, 10_000);
    assert_eq!(page.admin, StatsPageAdminPolicy::Localhost);
    assert_eq!(page.max_connections, Some(250));
    assert_eq!(page.downstream_timeouts.client_timeout_ms, Some(600_000));
    assert_eq!(page.downstream_timeouts.request_timeout_ms, Some(600_000));
    assert_eq!(page.downstream_timeouts.keepalive_timeout_ms, Some(60_000));

    let rendered = render_lua(&config).expect("render page");
    let roundtrip = load_lua(&rendered).expect("roundtrip page");
    assert_eq!(roundtrip, config);
}

#[test]
fn statistics_page_uri_prefix_requires_an_exact_ascii_http_path() {
    for uri in ["/café1", "/bad|path", "/bad^path"] {
        let source = format!(
            r#"return {{
              version = 1,
              listeners = {{}},
              stats = {{ pages = {{{{
                bind = "127.0.0.1:8404",
                uri_prefix = "{uri}",
                refresh_ms = 1000,
                admin = "disabled",
              }}}} }},
            }}"#
        );
        assert!(matches!(
            load_lua(&source).expect_err("invalid statistics page URI"),
            ConfigError::InvalidStatsPage {
                field: "uri_prefix",
                ..
            }
        ));
    }
}

#[test]
fn rejects_invalid_statistics_pages_and_broad_bind_conflicts() {
    for (field, value) in [
        ("uri_prefix", "uri_prefix = \"relative\", refresh_ms = 1000"),
        ("refresh_ms", "uri_prefix = \"/stats\", refresh_ms = 0"),
        (
            "refresh_ms",
            "uri_prefix = \"/stats\", refresh_ms = 86400001",
        ),
    ] {
        let source = format!(
            r#"return {{
              version = 1,
              listeners = {{}},
              stats = {{
                pages = {{{{
                  bind = "127.0.0.1:8404",
                  {value},
                  admin = "disabled",
                }}}},
              }},
            }}"#
        );
        let error = load_lua(&source).expect_err("invalid page");
        assert!(
            matches!(error, ConfigError::InvalidStatsPage { field: actual, .. } if actual == field)
        );
    }

    let zero_limit = load_lua(
        r#"return {
          version = 1,
          listeners = {},
          stats = { pages = {{
            bind = "127.0.0.1:8404",
            uri_prefix = "/stats",
            refresh_ms = 1000,
            admin = "disabled",
            max_connections = 0,
          }} },
        }"#,
    )
    .expect_err("zero statistics page limit");
    assert!(matches!(zero_limit, ConfigError::ZeroLimit { .. }));

    let zero_timeout = load_lua(
        r#"return {
          version = 1,
          listeners = {},
          stats = { pages = {{
            bind = "127.0.0.1:8404",
            uri_prefix = "/stats",
            refresh_ms = 1000,
            admin = "disabled",
            downstream_timeouts = { request_timeout_ms = 0 },
          }} },
        }"#,
    )
    .expect_err("zero statistics page timeout");
    assert!(matches!(
        zero_timeout,
        ConfigError::InvalidStatsPage {
            field: "downstream_timeouts.request_timeout_ms",
            ..
        }
    ));

    let error = load_lua(
        r#"return {
          version = 1,
          listeners = {},
          stats = {
            binds = { "127.0.0.1:8404" },
            pages = {{
              bind = "0.0.0.0:8404",
              uri_prefix = "/stats",
              refresh_ms = 1000,
              admin = "disabled",
            }},
          },
        }"#,
    )
    .expect_err("broad page bind conflict");
    assert!(matches!(error, ConfigError::OverlappingBind { .. }));
}

#[test]
fn statistics_total_bind_count_is_bounded_across_legacy_and_page_binds() {
    let pages = (1..=8)
        .map(|port| {
            format!(
                "{{ bind = \"127.0.0.1:{}\", uri_prefix = \"/stats\", refresh_ms = 1000, admin = \"disabled\" }}",
                8404 + port
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let source = format!(
        r#"return {{
          version = 1,
          listeners = {{}},
          stats = {{ binds = {{ "127.0.0.1:8404" }}, pages = {{ {pages} }} }},
        }}"#
    );

    assert!(matches!(
        load_lua(&source).expect_err("nine total binds"),
        ConfigError::InvalidStatsBinds
    ));
}

#[test]
fn statistics_page_uri_prefix_is_bounded() {
    let uri = format!("/{}", "a".repeat(2_048));
    let source = format!(
        r#"return {{
          version = 1,
          listeners = {{}},
          stats = {{ pages = {{{{
            bind = "127.0.0.1:8404",
            uri_prefix = "{uri}",
            refresh_ms = 1000,
            admin = "disabled",
          }}}} }},
        }}"#
    );

    assert!(matches!(
        load_lua(&source).expect_err("oversized page URI"),
        ConfigError::InvalidStatsPage {
            field: "uri_prefix",
            ..
        }
    ));
}

#[test]
fn statistics_pages_conflict_with_management_and_other_broad_pages() {
    for stats in [
        r#"management = { bind = "127.0.0.1:8404" },
           stats = { pages = {{
             bind = "0.0.0.0:8404", uri_prefix = "/stats", refresh_ms = 1000, admin = "disabled",
           }} },"#,
        r#"stats = { pages = {
             { bind = "127.0.0.1:8404", uri_prefix = "/one", refresh_ms = 1000, admin = "disabled" },
             { bind = "0.0.0.0:8404", uri_prefix = "/two", refresh_ms = 1000, admin = "disabled" },
           } },"#,
    ] {
        let source = format!(
            r"return {{
              version = 1,
              listeners = {{}},
              {stats}
            }}"
        );
        assert!(matches!(
            load_lua(&source).expect_err("page bind overlap"),
            ConfigError::OverlappingBind { .. }
        ));
    }

    let error = load_lua(
        r#"return {
          version = 1,
          stats = { pages = {{
            bind = "0.0.0.0:8404", uri_prefix = "/stats", refresh_ms = 1000, admin = "disabled",
          }} },
          listeners = {{
            name = "web",
            bind = { type = "socket", address = "127.0.0.1:8404" },
            protocol = "http",
            service = "web",
          }},
          http_services = {{
            name = "web",
            routes = {{
              path = { kind = "exact", value = "/" },
              action = { type = "fixed_response", status = 200 },
            }},
          }},
        }"#,
    )
    .expect_err("page/listener overlap");
    assert!(matches!(error, ConfigError::OverlappingBind { .. }));
}
