use std::{fmt::Write as _, time::Duration};

use oxiroute_import::haproxy::{
    AclCriterion, BalanceAlgorithm, BindAddress, E_LOGGING_UNSUPPORTED, E_PROCESS_OWNED,
    E_STATS_UNSUPPORTED, E_UNCONSUMED_DIRECTIVE, EffectiveConfiguration, OptionState, ProxyMode,
    SemanticBlockerKind, ServerAddress,
};

use super::common::{code_count, occurrence_count, resolve_bytes};

#[test]
fn resolves_unix_frontend_and_dns_backend_without_lowering() {
    let contents = b"global
  log /dev/log local0
  chroot /var/lib/haproxy
  user haproxy
  group haproxy
  daemon
  maxconn 4096
  stats socket /run/haproxy/admin.sock
defaults shared
  log global
  mode http
  option httplog
  option forwardfor except 127.0.0.0/8
  option httpchk GET /healthz
  http-check expect status 200
  retries 3
  option redispatch
  timeout connect 5s
  timeout client 50s
  timeout server 50s
  timeout http-request 10s
  timeout http-keep-alive 10s
frontend host-a
  bind /run/haproxy/host-a.sock
  maxconn 2000
  acl host_app hdr(host) -i app.test
  use_backend app_nodes if host_app
  default_backend app_nodes
backend app_nodes
  balance leastconn
  server app1 app1.test:3000 check inter 2s rise 2 fall 3
  server app2 app2.test:3000 check inter 2s rise 2 fall 3
";
    let (configuration, report) = resolve_bytes(contents);

    assert_eq!(report.value().global.maxconn.as_ref().unwrap().value, 4096);
    assert_eq!(report.value().frontends.len(), 1);
    assert_eq!(report.value().backends.len(), 1);
    assert_unix_frontend(report.value());
    assert_dns_backend(report.value());

    assert_eq!(code_count(&report, E_LOGGING_UNSUPPORTED), 3);
    assert_eq!(code_count(&report, E_PROCESS_OWNED), 4);
    assert_eq!(code_count(&report, E_STATS_UNSUPPORTED), 1);
    assert_eq!(
        report.value().ledger.len(),
        occurrence_count(&configuration)
    );
    assert_eq!(code_count(&report, E_UNCONSUMED_DIRECTIVE), 0);
}

#[test]
fn unix_at_bind_and_octal_mode_are_typed_with_direct_provenance() {
    let contents = b"frontend public
  mode http
  bind unix@/run/haproxy/public.sock mode 777
";
    let (_, report) = resolve_bytes(contents);
    let bind = &report.value().frontends[0].binds[0];

    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    assert_eq!(
        bind.address.value,
        BindAddress::Unix {
            path: b"/run/haproxy/public.sock".to_vec()
        }
    );
    assert_eq!(bind.mode.as_ref().expect("Unix mode").value, 0o777);
    assert!(
        bind.mode
            .as_ref()
            .expect("Unix mode")
            .provenance
            .is_direct()
    );
}

#[test]
fn invalid_or_non_unix_bind_modes_are_rejected() {
    for bind in [
        "unix@/run/haproxy/public.sock mode 888",
        "unix@/run/haproxy/public.sock mode 000",
        "127.0.0.1:8080 mode 777",
    ] {
        let contents = format!("frontend public\n  mode http\n  bind {bind}\n");
        let (_, report) = resolve_bytes(contents.as_bytes());

        assert!(
            report.has_errors(),
            "invalid bind mode unexpectedly resolved: {bind}"
        );
        assert!(report.value().frontends[0].binds.is_empty());
    }
}

fn assert_unix_frontend(configuration: &EffectiveConfiguration) {
    let frontend = &configuration.frontends[0];
    assert_eq!(
        frontend.binds[0].value,
        BindAddress::Unix {
            path: b"/run/haproxy/host-a.sock".to_vec()
        }
    );
    assert_eq!(
        frontend.settings.mode.as_ref().unwrap().value,
        ProxyMode::Http
    );
    assert!(
        frontend
            .settings
            .mode
            .as_ref()
            .unwrap()
            .provenance
            .is_inherited()
    );
    assert_eq!(frontend.settings.maxconn.as_ref().unwrap().value, 2000);
    assert_eq!(frontend.acls[0].value.criterion, AclCriterion::HostExact);
    assert!(frontend.acls[0].value.case_insensitive);
    assert_eq!(frontend.use_backends.len(), 1);
    assert_eq!(frontend.use_backends[0].value.backend.name, b"app_nodes");
    assert_eq!(
        frontend
            .settings
            .default_backend
            .as_ref()
            .unwrap()
            .value
            .name,
        b"app_nodes"
    );
    assert!(
        frontend
            .settings
            .default_backend
            .as_ref()
            .unwrap()
            .provenance
            .is_reference()
    );
}

fn assert_dns_backend(configuration: &EffectiveConfiguration) {
    let backend = &configuration.backends[0];
    assert_eq!(
        backend.settings.balance.as_ref().unwrap().value,
        BalanceAlgorithm::LeastConnections
    );
    assert_eq!(backend.settings.retries.as_ref().unwrap().value, 3);
    assert_eq!(
        backend.settings.timeouts.connect.as_ref().unwrap().value,
        Duration::from_secs(5)
    );
    assert_eq!(
        backend.settings.timeouts.server.as_ref().unwrap().value,
        Duration::from_secs(50)
    );
    assert_eq!(
        backend.settings.http_check_expect.as_ref().unwrap().value[0].start,
        200
    );
    assert!(matches!(
        backend.settings.redispatch.as_ref().unwrap().value,
        OptionState::Enabled(_)
    ));
    assert!(matches!(
        backend.settings.forward_for.as_ref().unwrap().value,
        OptionState::Enabled(ref forward_for)
            if forward_for.except.as_deref() == Some(b"127.0.0.0/8".as_slice())
    ));
    assert_eq!(backend.servers.len(), 2);
    assert_eq!(
        backend.servers[0].address.value,
        ServerAddress::Tcp {
            host: b"app1.test".to_vec(),
            port: 3000
        }
    );
    assert_eq!(
        backend.servers[0].interval.as_ref().unwrap().value,
        Duration::from_secs(2)
    );
    assert_eq!(backend.servers[0].rise.as_ref().unwrap().value, 2);
    assert_eq!(backend.servers[0].fall.as_ref().unwrap().value, 3);
}

#[test]
fn resolves_dormant_shape_with_eight_servers() {
    let mut contents = String::from(
        "defaults host-c\n  mode http\n  option forwardfor\n  timeout connect 2s\n  timeout client 30s\n  timeout server 30s\nfrontend host-c\n  bind /run/haproxy/host-c.sock\n  acl host-c_host hdr(host) -i host-c.test\n  use_backend host-c_nodes if host-c_host\nbackend host-c_nodes\n  balance leastconn\n",
    );
    for ordinal in 1..=8 {
        writeln!(
            contents,
            "  server host-c{ordinal} host-c{ordinal}.test:4000 check inter 3s rise 2 fall 3"
        )
        .unwrap();
    }
    let (_, report) = resolve_bytes(contents.as_bytes());

    assert!(report.diagnostics().is_empty());
    assert_eq!(report.value().backends[0].servers.len(), 8);
    assert_eq!(
        report.value().frontends[0].use_backends[0]
            .value
            .backend
            .name,
        b"host-c_nodes"
    );
    assert!(
        report.value().backends[0]
            .settings
            .forward_for
            .as_ref()
            .unwrap()
            .provenance
            .is_inherited()
    );
}

#[test]
fn resolves_listen_as_one_combined_frontend_and_backend_occurrence() {
    let contents = b"defaults tcp_defaults
  mode tcp
  timeout connect 2s
  timeout client 30s
  timeout server 30s
listen postgres
  bind :5432
  balance roundrobin
  maxconn 128
  server primary postgres.test:5432 check inter 5s rise 2 fall 3
";
    let (_, report) = resolve_bytes(contents);

    assert!(report.diagnostics().is_empty());
    assert_eq!(report.value().listens.len(), 1);
    let listen = &report.value().listens[0];
    assert_eq!(listen.settings.mode.as_ref().unwrap().value, ProxyMode::Tcp);
    assert_eq!(
        listen.settings.balance.as_ref().unwrap().value,
        BalanceAlgorithm::RoundRobin
    );
    assert_eq!(listen.settings.maxconn.as_ref().unwrap().value, 128);
    assert_eq!(
        listen.binds[0].value,
        BindAddress::Tcp {
            host: Vec::new(),
            port: 5432,
        }
    );
    assert_eq!(listen.servers.len(), 1);
}

#[test]
fn unsupported_modes_are_inherited_as_blockers_instead_of_falling_back_to_http() {
    let contents = b"defaults shared
  mode health
frontend public
  bind 127.0.0.1:8080
";
    let (_, report) = resolve_bytes(contents);
    let mode = report.value().frontends[0]
        .settings
        .mode
        .as_ref()
        .expect("retained unsupported mode");

    assert_eq!(mode.value, ProxyMode::Unsupported(b"health".to_vec()));
    assert!(mode.provenance.is_inherited());
    assert!(
        report.value().frontends[0]
            .settings
            .semantic_blockers
            .iter()
            .any(|blocker| blocker.value.kind == SemanticBlockerKind::Mode)
    );
}
