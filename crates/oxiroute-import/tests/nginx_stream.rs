#![cfg(unix)]

use std::{fs, net::SocketAddr, path::Path};

use oxiroute_config::{ListenerBind, Protocol, ProxyProtocolVersion, UpstreamEndpoint};
use oxiroute_import::nginx::{
    OccurrenceDisposition, RootOccurrenceDisposition, StreamDeclaration, StreamDestination,
    import_root, import_stream_fragment, load, resolve_stream_fragment,
};

#[test]
fn resolves_static_stream_upstreams_and_forward_references() {
    let resolved = resolve_source(
        br"
            stream {
                server { listen 15432; proxy_pass postgres; }
                upstream postgres {
                    server db.internal:5432;
                    server unix:/run/backup.sock;
                }
            }
        ",
    );

    assert!(
        resolved.diagnostics().is_empty(),
        "{:#?}",
        resolved.diagnostics()
    );
    let stream = &resolved.value().stream_blocks[0];
    assert_eq!(stream.upstreams.len(), 1);
    assert_eq!(stream.servers.len(), 1);
    assert_eq!(
        stream.declaration_order,
        [
            StreamDeclaration::Server(stream.servers[0].origin.occurrence),
            StreamDeclaration::Upstream(stream.upstreams[0].origin.occurrence),
        ]
    );
    assert_eq!(
        stream.servers[0].proxy_pass.as_ref().unwrap().destination,
        StreamDestination::Upstream(stream.upstreams[0].origin.occurrence)
    );
    assert!(matches!(
        stream.upstreams[0].servers[0].endpoint,
        Some(oxiroute_import::nginx::StaticEndpoint::Dns { ref host, port })
            if host == "db.internal" && port == 5432
    ));
    assert!(matches!(
        stream.upstreams[0].servers[1].endpoint,
        Some(oxiroute_import::nginx::StaticEndpoint::Unix { ref path })
            if path == Path::new("/run/backup.sock")
    ));
}

#[test]
fn lowers_stream_tcp_service_with_inherited_and_local_timeouts() {
    let directory = tempfile::tempdir().expect("stream fixture directory");
    fs::write(
        directory.path().join("nginx.conf"),
        br"
            stream {
                proxy_connect_timeout 7s;
                proxy_timeout 11m;
                upstream postgres { server 127.0.0.1:5432; }
                server {
                    listen 127.0.0.1:15432 default_server;
                    proxy_timeout 13s;
                    proxy_pass postgres;
                }
            }
        ",
    )
    .expect("write stream fixture");

    let report = import_stream_fragment(Path::new("nginx.conf"), directory.path());
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    let config = report.config().expect("finalized stream config");
    assert_eq!(config.listeners.len(), 1);
    assert_eq!(config.listeners[0].protocol, Protocol::Tcp);
    assert_eq!(
        config.listeners[0].bind,
        ListenerBind::Socket {
            address: "127.0.0.1:15432".parse::<SocketAddr>().unwrap()
        }
    );
    assert_eq!(config.upstream_pools.len(), 1);
    assert_eq!(
        config.upstream_pools[0].servers[0].endpoint,
        UpstreamEndpoint::Socket {
            address: "127.0.0.1:5432".parse::<SocketAddr>().unwrap()
        }
    );
    assert_eq!(config.l4_services.len(), 1);
    assert_eq!(config.l4_services[0].connect_timeout_ms, 7_000);
    assert_eq!(config.l4_services[0].idle_timeout_ms, 13_000);
    assert_eq!(
        config.l4_services[0].upstream_pool,
        config.upstream_pools[0].name
    );
    assert!(report.provenance.iter().any(|entry| {
        entry.path == "/l4_services/0/connect_timeout_ms"
            && entry.origins.iter().any(|origin| {
                report.occurrence_ledger.iter().any(|decision| {
                    decision.name.value == b"proxy_connect_timeout"
                        && decision.occurrence == origin.occurrence
                })
            })
    }));
    assert_eq!(
        report
            .occurrence_ledger
            .iter()
            .filter(|decision| decision.disposition == OccurrenceDisposition::Resolved)
            .count(),
        report.source_graph.expanded_occurrences.len()
    );
}

#[test]
fn lowers_exact_stream_proxy_protocol_acceptance_and_propagation() {
    let directory = tempfile::tempdir().expect("stream PROXY fixture directory");
    fs::write(
        directory.path().join("nginx.conf"),
        br"
            stream {
                upstream postgres { server 127.0.0.1:5432; }
                server {
                    proxy_protocol on;
                    listen 127.0.0.1:15432 proxy_protocol;
                    proxy_pass postgres;
                }
            }
        ",
    )
    .expect("write stream PROXY fixture");
    let report = import_stream_fragment(Path::new("nginx.conf"), directory.path());

    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    let config = report.config().expect("stream PROXY configuration");
    assert_eq!(
        config.listeners[0]
            .proxy_protocol
            .expect("listener PROXY policy")
            .version,
        ProxyProtocolVersion::Auto
    );
    assert_eq!(
        config.l4_services[0]
            .proxy_protocol
            .expect("service PROXY policy")
            .version,
        ProxyProtocolVersion::V1
    );
    assert!(
        report
            .provenance
            .iter()
            .any(|entry| entry.path == "/l4_services/0/proxy_protocol/version")
    );
}

#[test]
fn preserves_include_graph_and_provenance_for_stream_upstream_lowering() {
    let directory = tempfile::tempdir().expect("stream include fixture directory");
    fs::write(
        directory.path().join("nginx.conf"),
        br"
            stream {
                include stream-upstreams.conf;
                server { listen 15432; proxy_pass database; }
            }
        ",
    )
    .expect("write stream include root");
    fs::write(
        directory.path().join("stream-upstreams.conf"),
        b"upstream database { server 127.0.0.1:5432; }\n",
    )
    .expect("write stream include target");

    let report = import_stream_fragment(Path::new("nginx.conf"), directory.path());
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    let included_server = report
        .occurrence_ledger
        .iter()
        .find(|decision| {
            decision.name.value == b"server" && decision.provenance.include_stack.len() == 1
        })
        .expect("included stream upstream server decision");
    assert_eq!(report.source_graph.includes.len(), 1);
    assert!(report.provenance.iter().any(|entry| {
        entry.path == "/upstream_pools/0/servers/0/endpoint/address"
            && entry
                .origins
                .iter()
                .any(|origin| origin.occurrence == included_server.occurrence)
    }));
}

#[test]
fn lowers_unix_stream_listener_and_direct_unix_proxy_destination() {
    let directory = tempfile::tempdir().expect("stream Unix fixture directory");
    fs::write(
        directory.path().join("nginx.conf"),
        b"stream { server { listen unix:/run/oxiroute-stream.sock; proxy_pass unix:/run/database.sock; } }",
    )
    .expect("write Unix stream fixture");

    let report = import_stream_fragment(Path::new("nginx.conf"), directory.path());
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    let config = report.config().expect("Unix stream config");
    assert_eq!(
        config.listeners[0].bind,
        ListenerBind::Unix {
            path: "/run/oxiroute-stream.sock".into(),
            mode: None,
        }
    );
    assert_eq!(
        config.upstream_pools[0].servers[0].endpoint,
        UpstreamEndpoint::Unix {
            path: "/run/database.sock".into(),
        }
    );
}

#[test]
fn blocks_udp_preread_and_dynamic_stream_routing() {
    let directory = tempfile::tempdir().expect("blocked stream fixture directory");
    fs::write(
        directory.path().join("nginx.conf"),
        br"
            stream {
                map $ssl_preread_server_name $backend { default 127.0.0.1:5432; }
                server {
                    listen 15432 udp;
                    ssl_preread on;
                    proxy_pass $backend;
                }
            }
        ",
    )
    .expect("write blocked stream fixture");

    let report = import_stream_fragment(Path::new("nginx.conf"), directory.path());
    assert!(report.has_errors());
    assert!(report.config().is_none());
    assert_eq!(report.blocked_services.len(), 1);
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message()
            .contains("dynamic stream routing is outside")
    }));
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message().contains("UDP listeners are outside") })
    );
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message().contains("TLS preread is unsupported") })
    );
}

#[test]
fn complete_root_merges_stream_services_and_marks_stream_occurrences() {
    let directory = tempfile::tempdir().expect("complete stream root directory");
    fs::write(
        directory.path().join("nginx.conf"),
        br"
            events { worker_connections 64; }
            http { access_log off; server { listen 18080; location / { return 200 ok; } } }
            stream {
                upstream database { server 127.0.0.1:5432; }
                server { listen 15432; proxy_pass database; }
            }
        ",
    )
    .expect("write complete stream root");

    let report = import_root(Path::new("nginx.conf"), directory.path());
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    let config = report.candidate.config().expect("merged config");
    assert_eq!(config.listeners.len(), 2);
    assert_eq!(config.l4_services.len(), 1);
    assert!(report.blocked_stream_services.is_empty());
    assert!(
        report
            .root_occurrence_ledger
            .iter()
            .any(|decision| matches!(decision.disposition, RootOccurrenceDisposition::Stream))
    );
    assert!(
        report
            .candidate
            .provenance
            .iter()
            .any(|entry| entry.path == "/listeners/1")
    );
}

fn resolve_source(
    source: &[u8],
) -> oxiroute_import::Report<oxiroute_import::nginx::StreamResolution> {
    let directory = tempfile::tempdir().expect("stream semantic directory");
    fs::write(directory.path().join("nginx.conf"), source).expect("write stream source");
    resolve_stream_fragment(load(Path::new("nginx.conf"), directory.path()))
}
