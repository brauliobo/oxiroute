use oxiroute_config::AccessLogPolicy;
use oxiroute_import::E_SEMANTICS_NOT_REPRESENTABLE;

use super::support::import_source;

#[test]
fn lowers_rtmp_message_and_acknowledgement_limits_with_provenance() {
    let report = import_source(
        br"
        rtmp {
          max_message 2m;
          ack_window 1000000;
          server {
            listen 127.0.0.1:1935;
            application live { live on; }
          }
        }
        ",
        &[],
    );

    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("RTMP transport limits");
    let service = &config.rtmp_services[0];
    assert_eq!(service.max_inbound_message_size, 2 * 1024 * 1024);
    assert_eq!(service.ack_window_size, 1_000_000);
    assert!(
        report
            .provenance
            .iter()
            .any(|entry| { entry.path == "/rtmp_services/0/max_inbound_message_size" })
    );
    assert!(
        report
            .provenance
            .iter()
            .any(|entry| { entry.path == "/rtmp_services/0/ack_window_size" })
    );
}

#[test]
fn lowers_server_scoped_rtmp_message_and_acknowledgement_limits() {
    let report = import_source(
        br"
        rtmp {
          server {
            listen 127.0.0.1:1935;
            max_message 3m;
            ack_window 2000000;
            application live { live on; }
          }
        }
        ",
        &[],
    );

    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("server-scoped RTMP limits");
    let service = &config.rtmp_services[0];
    assert_eq!(service.max_inbound_message_size, 3 * 1024 * 1024);
    assert_eq!(service.ack_window_size, 2_000_000);
}

#[test]
fn server_scoped_rtmp_limits_override_inherited_values() {
    let report = import_source(
        br"
        rtmp {
          max_message 1m;
          ack_window 1000000;
          server {
            listen 127.0.0.1:1935;
            max_message 3m;
            ack_window 2000000;
            application live { live on; }
          }
        }
        ",
        &[],
    );

    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("server override of RTMP limits");
    let service = &config.rtmp_services[0];
    assert_eq!(service.max_inbound_message_size, 3 * 1024 * 1024);
    assert_eq!(service.ack_window_size, 2_000_000);
}

#[test]
fn blocks_nonuniform_effective_rtmp_limits_across_servers() {
    let report = import_source(
        br"
        rtmp {
          server {
            listen 127.0.0.1:1935;
            application first { live on; }
          }
          server {
            listen 127.0.0.1:1936;
            max_message 2m;
            ack_window 2000000;
            application second { live on; }
          }
        }
        ",
        &[],
    );

    assert!(
        report
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == E_SEMANTICS_NOT_REPRESENTABLE
            && diagnostic.message().contains("max_message")
    }));
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == E_SEMANTICS_NOT_REPRESENTABLE
            && diagnostic.message().contains("ack_window")
    }));
}

#[test]
fn rejects_rtmp_transport_limits_outside_canonical_bounds() {
    let report = import_source(
        br"
        rtmp {
          max_message 9m;
          ack_window 0;
          server {
            listen 127.0.0.1:1935;
            application live { live on; }
          }
        }
        ",
        &[],
    );

    assert!(
        report
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == oxiroute_import::E_INVALID_VALUE
            && diagnostic.message().contains("max_message")
    }));
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == oxiroute_import::E_INVALID_VALUE
            && diagnostic.message().contains("ack_window")
    }));
}

#[test]
fn lowers_one_absolute_rtmp_access_log_with_the_combined_format() {
    let report = import_source(
        br"
        rtmp {
          access_log /var/log/oxiroute/rtmp-access.jsonl combined;
          server {
            listen 127.0.0.1:1935;
            application live { live on; }
          }
        }
        ",
        &[],
    );

    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("exact RTMP access log configuration");
    assert!(report.blocked_services.is_empty());
    assert_eq!(
        config.rtmp_services[0].access_log,
        Some(AccessLogPolicy::File {
            path: "/var/log/oxiroute/rtmp-access.jsonl".into(),
        })
    );
}

#[test]
fn lowers_bounded_access_rules_and_application_connection_ceiling() {
    let report = import_source(
        br"
        rtmp {
          server {
            listen 127.0.0.1:1935;
            application camera {
              live on;
              allow publish 192.0.2.0/24;
              deny publish all;
              deny play 198.51.100.0/24;
              allow play all;
              max_connections 64;
            }
          }
        }
        ",
        &[],
    );

    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("bounded RTMP policy");
    let application = &config.rtmp_services[0].applications[0];
    assert_eq!(
        application.publish.rules[0].action,
        oxiroute_config::RtmpAclAction::Allow
    );
    assert_eq!(application.publish.rules[0].network, "192.0.2.0/24");
    assert_eq!(
        application.publish.rules[1].action,
        oxiroute_config::RtmpAclAction::Deny
    );
    assert_eq!(application.publish.rules[1].network, "all");
    assert_eq!(application.play.rules.len(), 2);
    assert_eq!(application.limits.max_connections, 64);
    assert_eq!(application.limits.max_publishers, 256);
    assert_eq!(application.limits.max_viewers, 1_024);
    assert!(
        report
            .provenance
            .iter()
            .any(|entry| { entry.path == "/rtmp_services/0/applications/0/publish/rules/0" })
    );
    assert!(
        report
            .provenance
            .iter()
            .any(|entry| { entry.path == "/rtmp_services/0/applications/0/limits" })
    );
}
