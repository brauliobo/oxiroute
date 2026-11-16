use oxiroute_import::{
    Report,
    haproxy::{
        CanonicalCandidate, E_STATS_UNSUPPORTED, HaproxyImportOptions,
        HaproxyPrometheusMigrationOverlay,
    },
};

use super::common::{
    analyze_fixture, assert_has_provenance, code_count, diagnostic_contains, import_fixture,
    loaded_fixture, validated_config,
};

#[test]
fn prometheus_service_retains_migration_requirement_without_an_operator_overlay() {
    let source = b"defaults web
  mode http
  retries 0
  timeout connect 5s
  timeout server 30s
frontend metrics
  bind 127.0.0.1:8404
  http-request use-service prometheus-exporter if { path /metrics }
frontend app
  bind 127.0.0.1:8080 maxconn 10
  maxconn 10
  default_backend workers
backend workers
  balance roundrobin
  server app1 127.0.0.1:3000
";

    let imported = import_fixture("prometheus-activation.cfg", source);
    let candidate = imported.value();
    let config = candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("unrelated app config");
    assert_eq!(config.listeners.len(), 1);
    assert!(config.stats.is_none());
    assert_eq!(candidate.activation_requirements.len(), 1);
    assert_eq!(
        candidate.activation_requirements[0].kind,
        oxiroute_import::ActivationRequirementKind::PrometheusExporter
    );
    assert!(!candidate.activation_requirements[0].equivalent_runtime_endpoint);
    assert_eq!(code_count(imported.diagnostics(), E_STATS_UNSUPPORTED), 1);
}

#[test]
fn dedicated_supported_stats_sections_lower_only_to_canonical_pages() {
    let source = b"frontend stats
  mode http
  bind *:8404
  maxconn 300
  timeout client 30s
  timeout http-request 5s
  timeout http-keep-alive 2s
  stats uri /stats
  stats refresh 10s
  stats admin if LOCALHOST
listen public-stats
  mode http
  bind 127.0.0.1:8405
  stats enable
  stats uri /public
  stats refresh 5s
";

    let imported = import_fixture("stats-pages.cfg", source);
    assert!(!imported.has_errors(), "{:?}", imported.diagnostics());
    assert_eq!(code_count(imported.diagnostics(), E_STATS_UNSUPPORTED), 0);
    let candidate = imported.value();
    let config = candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("stats pages config");
    let stats = config.stats.as_ref().expect("canonical stats");
    assert!(stats.binds.is_empty());
    assert_eq!(stats.pages.len(), 2);
    assert_eq!(stats.pages[0].bind, "0.0.0.0:8404".parse().unwrap());
    assert_eq!(stats.pages[0].uri_prefix, "/stats");
    assert_eq!(stats.pages[0].refresh_ms, 10_000);
    assert_eq!(stats.pages[0].max_connections, Some(300));
    assert_eq!(
        stats.pages[0].downstream_timeouts.client_timeout_ms,
        Some(30_000)
    );
    assert_eq!(
        stats.pages[0].downstream_timeouts.request_timeout_ms,
        Some(5_000)
    );
    assert_eq!(
        stats.pages[0].downstream_timeouts.keepalive_timeout_ms,
        Some(2_000)
    );
    assert_eq!(
        stats.pages[0].admin,
        oxiroute_config::StatsPageAdminPolicy::Localhost
    );
    assert_eq!(
        stats.pages[1].admin,
        oxiroute_config::StatsPageAdminPolicy::Disabled
    );
    assert!(config.listeners.is_empty());
    assert!(config.http_services.is_empty());
    assert!(config.l4_services.is_empty());
    assert!(candidate.activation_requirements.is_empty());
    for path in [
        "/stats/pages/0",
        "/stats/pages/0/bind",
        "/stats/pages/0/uri_prefix",
        "/stats/pages/0/refresh_ms",
        "/stats/pages/0/admin",
        "/stats/pages/0/max_connections",
        "/stats/pages/0/downstream_timeouts/client_timeout_ms",
        "/stats/pages/0/downstream_timeouts/request_timeout_ms",
        "/stats/pages/0/downstream_timeouts/keepalive_timeout_ms",
    ] {
        assert_has_provenance(candidate, path);
    }
}

#[test]
fn stats_frontend_response_rules_fail_closed_instead_of_disappearing() {
    let source = b"frontend stats
  mode http
  bind 127.0.0.1:8404
  stats uri /stats
  stats refresh 10s
  http-response set-header x-stats-policy retained
";

    let imported = import_fixture("stats-response-policy.cfg", source);

    assert!(
        imported
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert!(!imported.value().summary().stats);
    assert!(diagnostic_contains(
        imported.diagnostics(),
        "stats frontend response rules"
    ));
}

#[test]
fn stats_frontend_connection_close_policy_fails_closed_instead_of_disappearing() {
    let source = b"frontend stats
  mode http
  bind 127.0.0.1:8404
  option http-server-close
  stats uri /stats
  stats refresh 10s
";

    let imported = import_fixture("stats-connection-policy.cfg", source);

    assert!(
        imported
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert!(!imported.value().summary().stats);
    assert!(imported.has_errors());
}

#[test]
fn unsupported_stats_form_suppresses_page_without_creating_an_ordinary_service() {
    let source = b"frontend stats
  mode http
  bind *:8404
  stats enable
  stats uri /stats
  stats refresh 10s
  stats hide-version
";

    let imported = import_fixture("blocked-stats-page.cfg", source);
    let candidate = imported.value();
    assert_eq!(code_count(imported.diagnostics(), E_STATS_UNSUPPORTED), 1);
    assert_eq!(candidate.activation_requirements.len(), 1);
    assert!(!candidate.summary().stats);
    assert_eq!(candidate.summary().listeners, 0);
    assert_eq!(candidate.summary().http_services, 0);
}

#[test]
fn stats_auth_suppresses_page_without_creating_an_ordinary_service() {
    let source = b"frontend stats
  mode http
  bind *:8404
  stats uri /stats
  stats auth operator:secret
";

    let imported = import_fixture("authenticated-stats-page.cfg", source);
    let candidate = imported.value();
    assert_eq!(code_count(imported.diagnostics(), E_STATS_UNSUPPORTED), 1);
    assert!(!candidate.summary().stats);
    assert_eq!(candidate.summary().listeners, 0);
    assert_eq!(candidate.summary().http_services, 0);
}

#[test]
fn explicit_prometheus_migration_overlay_enables_the_distinct_oxiroute_stats_contract() {
    let source = b"frontend metrics
  bind 127.0.0.1:8404
  stats auth operator:secret
  stats uri /admin
  stats admin if TRUE
  http-request use-service prometheus-exporter if { path /metrics }
";

    let parsed = analyze_fixture("strict-stats-forms.cfg", source);
    let imported = import_fixture_with_options(
        "strict-stats-forms.cfg",
        source,
        &HaproxyImportOptions {
            one_request_per_connection: Vec::new(),
            prometheus_migrations: vec![HaproxyPrometheusMigrationOverlay {
                section: "metrics".into(),
            }],
        },
    );
    assert_eq!(
        parsed.value().supported_stats_sections.len(),
        1,
        "fixture must remain an exact native Prometheus service"
    );
    let candidate = imported.value();
    assert_eq!(
        validated_config(candidate)
            .stats
            .as_ref()
            .expect("exact Prometheus endpoint")
            .binds,
        ["127.0.0.1:8404".parse().unwrap()]
    );
    assert_eq!(candidate.activation_requirements.len(), 3);
    assert!(
        candidate
            .activation_requirements
            .iter()
            .all(|requirement| { !requirement.equivalent_runtime_endpoint })
    );
    assert!(candidate.operational_overlays.iter().any(|overlay| {
        overlay.kind == oxiroute_import::OperationalOverlayKind::PrometheusMigration
            && overlay.satisfied
    }));
    assert_eq!(code_count(imported.diagnostics(), E_STATS_UNSUPPORTED), 3);
}

#[test]
fn prometheus_migration_overlay_must_uniquely_match_a_dedicated_exact_service() {
    let source = b"frontend metrics
  bind 127.0.0.1:8404
  http-request use-service prometheus-exporter if { path /metrics }
";
    for sections in [vec!["missing"], vec!["metrics", "metrics"]] {
        let imported = import_fixture_with_options(
            "invalid-prometheus-migration.cfg",
            source,
            &HaproxyImportOptions {
                one_request_per_connection: Vec::new(),
                prometheus_migrations: sections
                    .into_iter()
                    .map(|section| HaproxyPrometheusMigrationOverlay {
                        section: section.into(),
                    })
                    .collect(),
            },
        );
        assert!(
            imported
                .value()
                .validated()
                .map(oxiroute_config::ValidatedConfig::as_draft)
                .is_none()
        );
        assert!(!imported.value().summary().stats);
        assert!(imported.value().operational_overlays.iter().all(|overlay| {
            overlay.kind != oxiroute_import::OperationalOverlayKind::PrometheusMigration
                || !overlay.satisfied
        }));
        assert!(diagnostic_contains(
            imported.diagnostics(),
            "must uniquely match one exact, dedicated Prometheus service"
        ));
    }
}

fn import_fixture_with_options(
    name: &str,
    contents: &[u8],
    options: &HaproxyImportOptions,
) -> Report<CanonicalCandidate> {
    oxiroute_import::haproxy::import_parsed_with_options(
        oxiroute_import::haproxy::parse_sources(&[loaded_fixture(name, contents)]),
        options,
    )
}
