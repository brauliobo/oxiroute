use std::{
    fs,
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
};

use oxiroute_config::{
    HttpPathSelector, HttpRouteAction, ListenerBind, UpstreamAlgorithm, UpstreamEndpoint,
};
use oxiroute_import::{
    SourceFile, SourceId,
    haproxy::{
        E_CONDITIONAL_PREPROCESSING, E_ENVIRONMENT_EXPANSION, E_UNKNOWN_DIRECTIVE,
        E_UNSUPPORTED_FORM, LoadedSource, PreprocessingEnvironment, import_roots_with_environment,
        import_sources,
    },
};
use tempfile::tempdir;

use super::common::{import_synthetic, validated_config};

#[test]
fn preprocessed_services_keep_health_routes_queue_timeouts_and_native_default_retries() {
    for (gpu1_defined, alpha2_port) in [(true, 3001), (false, 3002)] {
        let report = import_synthetic("env-health-routes.cfg", gpu1_defined);
        let candidate = report.value();
        let config = validated_config(candidate);

        assert_eq!(candidate.summary().max_connections, Some(1_024));
        assert_eq!(config.http_services.len(), 3);
        assert_eq!(config.upstream_pools.len(), 3);
        assert!(
            config
                .upstream_pools
                .iter()
                .all(|pool| pool.servers.len() == 2)
        );
        assert!(
            config
                .upstream_pools
                .iter()
                .all(|pool| pool.queue_timeout_ms == Some(1_800_000))
        );
        let alpha_servers = &config.upstream_pools[0].servers;
        assert!(matches!(
            alpha_servers[1].endpoint,
            UpstreamEndpoint::Socket { address } if address.port() == alpha2_port
        ));
        for service in &config.http_services {
            assert_eq!(service.routes.len(), 2);
            assert!(matches!(
                service.routes[0].path,
                HttpPathSelector::AsciiCaseInsensitiveExact { ref value } if value == "/_infra/health"
            ));
            assert!(matches!(
                service.routes[0].action,
                HttpRouteAction::FixedResponse { status: 200, ref body, .. } if body == "ok"
            ));
            let HttpRouteAction::Proxy { ref policy, .. } = service.routes[1].action else {
                panic!("fallback route must proxy")
            };
            assert_eq!(policy.retry.max_retries, 3);
        }
    }
}

#[test]
fn http_check_send_preprocessing_is_deterministic_and_retains_source_map() {
    let directory = tempdir().expect("preprocessing directory");
    let root = directory.path().join("haproxy.cfg");
    let source = b"defaults web
  mode http
  option httpchk
  timeout connect 5s
  timeout client 30s
  timeout server 30s
  default-server check inter 1s rise 1 fall 2
frontend public
  bind 127.0.0.1:18080
  default_backend app
backend app
  balance roundrobin
  http-check send meth GET uri /healthz ver HTTP/1.1 hdr Host backend.internal
  http-check expect status 204
  server app1 ${NODE_IP}:3000
";
    fs::write(&root, source).expect("preprocessing source");
    let environment = PreprocessingEnvironment {
        node_ip: "192.0.2.25".parse().expect("node IP"),
        gpu1_defined: false,
    };

    let first = import_roots_with_environment(std::slice::from_ref(&root), environment);
    let second = import_roots_with_environment(std::slice::from_ref(&root), environment);
    assert!(!first.has_errors(), "{:?}", first.diagnostics());
    assert_eq!(first.diagnostics(), second.diagnostics());
    assert_eq!(
        first
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft),
        second
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
    );
    assert_eq!(
        first.value().source_metadata,
        second.value().source_metadata
    );
    assert_eq!(first.value().source_metadata.original_sources.len(), 1);
    assert_eq!(
        first.value().source_metadata.original_sources[0].bytes(),
        source
    );
    assert_eq!(first.value().source_metadata.source_maps.len(), 1);
    assert!(
        first
            .value()
            .source_metadata
            .environment_fingerprint_sha256
            .is_some()
    );
}

#[test]
fn preprocessing_unknown_and_unsupported_semantics_flow_into_the_candidate_report() {
    let source = b".if defined(ENABLED)
defaults web
  mode http
  mystery value
  option magical
frontend public
  bind \"${BIND-127.0.0.1:8080}\"
  .endif
";
    let path = PathBuf::from("preprocessing.cfg");
    let lowered = import_sources(&[LoadedSource {
        root_ordinal: 0,
        file_ordinal: 0,
        source: SourceFile::from_path(SourceId::new(0), path.clone(), source.as_slice()),
        path,
    }]);

    assert!(
        lowered
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    for code in [
        E_CONDITIONAL_PREPROCESSING,
        E_ENVIRONMENT_EXPANSION,
        E_UNKNOWN_DIRECTIVE,
        E_UNSUPPORTED_FORM,
    ] {
        assert!(lowered.diagnostics().iter().any(|diagnostic| {
            diagnostic.code() == code && diagnostic.primary_span().is_some()
        }));
    }
}

#[test]
fn explicit_preprocessing_records_environment_and_inactive_gpu_provenance() {
    let directory = tempdir().expect("preprocessing fixture directory");
    let root = directory.path().join("haproxy.cfg");
    fs::write(
        &root,
        b"global\n  maxconn 64\n  log stdout format raw local0\n  user haproxy\ndefaults\n  mode tcp\n  retries 0\n  timeout connect 5s\n  timeout client 2h\n  timeout server 2h\nfrontend node\n  bind ${NODE_IP}:10440 maxconn 10\n  maxconn 10\n  default_backend workers\nbackend workers\n  balance first\n  option http-server-close\n  server gpu0 127.0.0.1:10450 maxconn 1 no-check\n.if defined(GPU1)\n  server gpu1 127.0.0.1:10451 maxconn 1 no-check\n.endif\n",
    )
    .expect("write preprocessing fixture");

    let without_gpu = import_roots_with_environment(
        &[&root],
        PreprocessingEnvironment {
            node_ip: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)),
            gpu1_defined: false,
        },
    );
    let candidate = without_gpu.value();
    let config = candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("preprocessed config");
    assert!(!config.listeners.is_empty(), "{without_gpu:#?}");
    assert_eq!(config.max_connections, Some(64));
    assert_eq!(
        config.listeners[0].bind,
        ListenerBind::Socket {
            address: "192.0.2.10:10440".parse().unwrap(),
            ipv6_only: false
        }
    );
    assert_eq!(config.upstream_pools[0].servers.len(), 1);
    assert_eq!(config.upstream_pools[0].algorithm, UpstreamAlgorithm::First);
    assert_eq!(config.upstream_pools[0].servers[0].max_connections, Some(1));
    assert_eq!(
        config.upstream_pools[0].connection_reuse,
        oxiroute_config::UpstreamConnectionReuse::Never
    );
    assert_eq!(candidate.deployment_requirements.len(), 2);
    assert_eq!(candidate.source_metadata.inactive_sources.len(), 1);
    assert_eq!(candidate.source_metadata.original_sources.len(), 1);
    assert_eq!(candidate.source_metadata.source_maps.len(), 1);
    assert_eq!(
        candidate.source_metadata.original_sources[0].bytes(),
        fs::read(&root).unwrap()
    );
    let source_map = &candidate.source_metadata.source_maps[0];
    let original = &candidate.source_metadata.original_sources[0];
    assert!(candidate.provenance.iter().any(|entry| {
        entry.origins.iter().any(|origin| {
            source_map
                .translate(origin.span)
                .and_then(|span| original.slice(span.range()))
                .is_some_and(|bytes| {
                    bytes
                        .windows(b"${NODE_IP}".len())
                        .any(|part| part == b"${NODE_IP}")
                })
        })
    }));
    assert_eq!(
        candidate.source_metadata.inactive_sources[0].condition,
        "defined(GPU1)"
    );
    let without_gpu_fingerprint = candidate
        .source_metadata
        .environment_fingerprint_sha256
        .as_deref()
        .expect("environment fingerprint");
    assert_eq!(without_gpu_fingerprint.len(), 64);

    let with_gpu = import_roots_with_environment(
        &[&root],
        PreprocessingEnvironment {
            node_ip: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)),
            gpu1_defined: true,
        },
    );
    let candidate = with_gpu.value();
    assert_eq!(
        candidate
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .expect("GPU config")
            .upstream_pools[0]
            .servers
            .len(),
        2
    );
    assert!(candidate.source_metadata.inactive_sources.is_empty());
    assert_ne!(
        candidate
            .source_metadata
            .environment_fingerprint_sha256
            .as_deref()
            .expect("GPU environment fingerprint"),
        without_gpu_fingerprint
    );
}

#[test]
fn preprocessing_is_deterministic_and_keeps_ordinary_source_snapshots() {
    let directory = tempdir().expect("deterministic preprocessing directory");
    let root = directory.path().join("haproxy.cfg");
    fs::write(
        &root,
        b"defaults web
  mode http
  retries 0
  timeout connect 5s
  timeout client 30s
  timeout server 30s
frontend public
  bind ${NODE_IP}:18080
  default_backend app
backend app
  balance roundrobin
  server app1 127.0.0.1:3000
.if defined(GPU1)
  server app2 127.0.0.1:3001
.endif
",
    )
    .expect("write deterministic preprocessing fixture");
    let environment = PreprocessingEnvironment {
        node_ip: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 20)),
        gpu1_defined: true,
    };

    let first = import_roots_with_environment(&[&root], environment);
    let second = import_roots_with_environment(&[&root], environment);
    assert_eq!(first.diagnostics(), second.diagnostics());
    assert_eq!(
        first
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft),
        second
            .value()
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
    );
    assert_eq!(
        first.value().source_metadata,
        second.value().source_metadata
    );
    assert_eq!(first.value().source_metadata.original_sources.len(), 1);
    assert_eq!(
        first.value().source_metadata.original_sources[0].bytes(),
        fs::read(&root).expect("original HAProxy source")
    );
    assert_eq!(first.value().source_metadata.source_maps.len(), 1);
    assert!(
        first
            .value()
            .source_metadata
            .environment_fingerprint_sha256
            .is_some()
    );
}
