use std::path::Path;

use oxiroute_config::RtmpHlsFragmentNaming;
use oxiroute_import::{
    E_INVALID_VALUE, E_SEMANTICS_NOT_REPRESENTABLE, E_UNSUPPORTED_FEATURE,
    nginx::OccurrenceDisposition,
};

use super::support::import_source;

#[test]
fn lowers_exact_same_daemon_auto_push_policy() {
    let report = import_source(
        br"
        rtmp_auto_push on;
        rtmp_auto_push_reconnect 250ms;
        rtmp_socket_dir /var/run/oxiroute/rtmp;
        rtmp {
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
        .expect("exact auto-push configuration");
    let policy = &config.rtmp_services[0].auto_push;
    assert!(policy.enabled);
    assert_eq!(policy.reconnect_ms, 250);
    assert_eq!(policy.socket_dir, Path::new("/var/run/oxiroute/rtmp"));
    assert!(
        report
            .provenance
            .iter()
            .any(|entry| { entry.path == "/rtmp_services/0/auto_push/enabled" })
    );
}

#[test]
fn lowers_bounded_push_and_blocks_pull_without_fallback() {
    let push = import_source(
        br"
        rtmp {
          server {
            listen 127.0.0.1:1935;
            application camera {
              live on;
              push rtmp://ORIGIN.Example:1940/$name;
            }
          }
        }
        ",
        &[],
    );
    let config = push
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("bounded push configuration");
    let target = &config.rtmp_services[0].applications[0].push_targets[0];
    assert_eq!(target.host, "origin.example");
    assert_eq!(target.port, 1_940);
    assert_eq!(target.application, "$name");
    assert!(
        push.provenance
            .iter()
            .any(|entry| { entry.path == "/rtmp_services/0/applications/0/push_targets/0" })
    );

    let pull = import_source(
        br"rtmp { server { listen 1935; application camera { pull rtmp://origin/live; } } }",
        &[],
    );
    assert!(
        pull.validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert!(pull.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == E_UNSUPPORTED_FEATURE && diagnostic.message().contains("relay")
    }));
    assert!(pull.occurrence_ledger.iter().any(|decision| {
        decision.name.value == b"pull"
            && decision.disposition == OccurrenceDisposition::Blocking(E_UNSUPPORTED_FEATURE)
    }));
}

#[test]
fn lowers_bounded_hls_policy_and_key_rotation() {
    let report = import_source(
        br"
        rtmp {
          hls on;
          hls_path /var/lib/oxiroute/hls;
          hls_fragment 2s;
          hls_max_fragment 6s;
          hls_playlist_length 30s;
          hls_nested on;
          hls_fragment_naming timestamp;
          hls_cleanup off;
          hls_keys on;
          hls_key_url keys/;
          hls_fragments_per_key 7;
          server {
            listen 127.0.0.1:1935;
            application camera { live on; }
          }
        }
        ",
        &[],
    );

    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("exact HLS configuration");
    assert!(report.blocked_services.is_empty());
    let hls = config.rtmp_services[0].applications[0]
        .hls
        .as_ref()
        .expect("HLS policy");
    assert_eq!(hls.root_directory, Path::new("/var/lib/oxiroute/hls"));
    assert_eq!(hls.segment_duration_ms, 2_000);
    assert_eq!(hls.max_segment_duration_ms, 6_000);
    assert_eq!(hls.playlist_length_ms, 30_000);
    assert_eq!(hls.fragment_naming, RtmpHlsFragmentNaming::Timestamp);
    assert!(hls.nested);
    assert!(!hls.cleanup);
    let keys = hls.keys.as_ref().expect("HLS keys");
    assert_eq!(keys.rotation_segments, 7);
    assert_eq!(keys.url_prefix, "keys/");
    assert!(
        report
            .provenance
            .iter()
            .any(|entry| { entry.path == "/rtmp_services/0/applications/0/hls" })
    );
}

#[test]
fn blocks_hls_logging_and_auto_push_forms_outside_the_bounded_subset() {
    for source in [
        br"rtmp { hls on; server { listen 1935; application app { live on; } } }".as_slice(),
        br"rtmp { hls_fragment 0; server { listen 1935; application app { live on; } } }".as_slice(),
        br"rtmp { access_log /var/log/rtmp.log custom; server { listen 1935; application app {} } }".as_slice(),
        br"rtmp_auto_push_reconnect 0ms; rtmp { server { listen 1935; application app {} } }".as_slice(),
        br"rtmp_socket_dir relative; rtmp { server { listen 1935; application app {} } }".as_slice(),
    ] {
        let report = import_source(source, &[]);
        assert!(report.validated().map(oxiroute_config::ValidatedConfig::as_draft).is_none(), "{source:?}");
        assert!(report.has_errors(), "{source:?}");
    }
    let invalid_hls_duration = import_source(
        br"rtmp { hls_playlist_length 0; server { listen 1935; application app { live on; } } }",
        &[],
    );
    assert!(invalid_hls_duration.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == E_INVALID_VALUE && diagnostic.message().contains("hls_playlist_length")
    }));
}

#[test]
fn blocks_unsafe_typed_exec_paths_without_shell_fallback() {
    for source in [
        br"rtmp { server { listen 1935; application app { exec_push relative/tool; } } }".as_slice(),
        br"rtmp { server { listen 1935; application app { exec_publish /usr/bin/tool arg<bad; } } }".as_slice(),
    ] {
        let report = import_source(source, &[]);
        assert!(report.validated().map(oxiroute_config::ValidatedConfig::as_draft).is_none(), "{source:?}");
        assert!(report.diagnostics.iter().any(|diagnostic| {
            diagnostic.code() == E_SEMANTICS_NOT_REPRESENTABLE
                || diagnostic.code() == E_INVALID_VALUE
        }));
    }
}

#[test]
fn lowers_allowlisted_exec_profiles_with_typed_arguments_and_provenance() {
    let report = import_source(
        br"
        rtmp {
          respawn on;
          respawn_timeout 2s;
          server {
            listen 127.0.0.1:1935;
            application camera {
              live on;
              exec_push /usr/bin/cat --input raw;
              exec_publish_done /usr/bin/true;
            }
          }
        }
        ",
        &[],
    );

    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("exact exec configuration");
    assert!(report.blocked_services.is_empty());
    let profiles = &config.rtmp_services[0].exec_profiles;
    assert_eq!(profiles.len(), 2);
    assert_eq!(profiles[0].application, "camera");
    assert_eq!(profiles[0].executable, Path::new("/usr/bin/cat"));
    assert_eq!(profiles[0].arguments, ["--input", "raw"]);
    assert_eq!(
        profiles[0].trigger,
        oxiroute_config::RtmpExecTrigger::Publisher
    );
    assert!(profiles[0].respawn);
    assert_eq!(profiles[0].respawn_delay_ms, 2_000);
    assert_eq!(
        profiles[1].trigger,
        oxiroute_config::RtmpExecTrigger::PublishDone
    );
    assert_eq!(profiles[1].working_directory, Path::new("/var/empty"));
    assert!(report.provenance.iter().any(|entry| {
        entry.path == "/rtmp_services/0/applications/0/exec_profiles/0/executable"
    }));
}

#[test]
fn source_noop_hls_muxdelay_does_not_block_an_exact_application() {
    let report = import_source(
        br"rtmp { server { listen 1935; application app { hls_muxdelay 700ms; } } }",
        &[],
    );

    assert!(
        report
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_some()
    );
    let decision = report
        .occurrence_ledger
        .iter()
        .find(|decision| decision.name.value == b"hls_muxdelay")
        .expect("hls_muxdelay decision");
    assert_eq!(decision.disposition, OccurrenceDisposition::Resolved);
}
