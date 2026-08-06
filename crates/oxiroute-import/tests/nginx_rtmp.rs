#![cfg(unix)]

use std::{fs, path::Path};

use oxiroute_config::{AccessLogPolicy, Protocol, RtmpHlsFragmentNaming, RtmpRecorderStart};
use oxiroute_import::{
    DiagnosticStage, E_DUPLICATE_IDENTITY, E_SEMANTICS_NOT_REPRESENTABLE, E_UNSUPPORTED_FEATURE,
    nginx::{OccurrenceDisposition, import_rtmp, import_rtmp_with_timezone, load, resolve_rtmp},
};
use tempfile::TempDir;

#[test]
fn lowers_inherited_exact_rtmp_and_recorder_policy_without_accessing_the_root() {
    let report = import_source(
        br"
        events {}
        rtmp {
          live on;
          idle_streams off;
          record all;
          record_path /definitely/not/an/import-time/root;
          record_suffix -%%.flv;
          record_unique on;
          record_interval 1s500ms;
          server {
            listen 127.0.0.1:1935;
            application phoenix {}
          }
        }
        ",
        &[],
    );

    let config = report.config.as_ref().expect("exact RTMP configuration");
    assert!(report.blocked_services.is_empty());
    assert_eq!(config.listeners.len(), 1);
    assert_eq!(config.listeners[0].protocol, Protocol::Rtmp);
    assert_eq!(config.listeners[0].bind.to_string(), "127.0.0.1:1935");
    assert_eq!(
        config.rtmp_services[0].max_inbound_message_size,
        1_024 * 1_024
    );
    assert_eq!(config.rtmp_services[0].ack_window_size, 5_000_000);
    let application = &config.rtmp_services[0].applications[0];
    assert_eq!(application.name, "phoenix");
    assert!(application.live);
    assert!(!application.idle_streams);
    let recorder = &application.recorders[0];
    assert_eq!(recorder.start, RtmpRecorderStart::Continuous);
    assert_eq!(
        recorder.root_directory,
        Path::new("/definitely/not/an/import-time/root")
    );
    assert_eq!(recorder.suffix_template, "-%%.flv");
    assert!(recorder.append_unix_seconds);
    assert_eq!(recorder.rotation_interval_ms, Some(1_500));
    assert_eq!(recorder.max_active_recorders, 32);
    assert_eq!(
        report.occurrence_ledger.len(),
        report.source_graph.expanded_occurrences.len()
    );
    assert!(report.occurrence_ledger.iter().all(|decision| {
        matches!(
            decision.disposition,
            OccurrenceDisposition::Resolved | OccurrenceDisposition::Structural
        )
    }));
    assert!(report.provenance.iter().any(|entry| {
        entry.path == "/rtmp_services/0/applications/0/live"
            && entry.origins[0].provenance.include_stack.is_empty()
    }));
}

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

    let config = report.config.expect("RTMP transport limits");
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

    let config = report.config.expect("server-scoped RTMP limits");
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

    let config = report.config.expect("server override of RTMP limits");
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

    assert!(report.config.is_none());
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

    assert!(report.config.is_none());
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

    let config = report.config.expect("exact auto-push configuration");
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

    let config = report.config.expect("exact RTMP access log configuration");
    assert!(report.blocked_services.is_empty());
    assert_eq!(
        config.rtmp_services[0].access_log,
        Some(AccessLogPolicy::File {
            path: "/var/log/oxiroute/rtmp-access.jsonl".into(),
        })
    );
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

    let config = report.config.expect("exact HLS configuration");
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

    let config = report.config.expect("exact exec configuration");
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

    let config = report.config.expect("bounded RTMP policy");
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

#[test]
fn recording_import_requires_an_explicit_host_iana_timezone() {
    let directory = TempDir::new().expect("RTMP source directory");
    fs::write(
        directory.path().join("nginx.conf"),
        b"rtmp { server { listen 127.0.0.1:1935; application live { live on; record all; record_path /srv/recordings; } } }",
    )
    .expect("write RTMP source");

    let report = import_rtmp(Path::new("nginx.conf"), directory.path());

    assert!(report.config.is_none());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message().contains("IANA timezone overlay") })
    );
}

#[test]
fn applies_native_application_and_recorder_defaults_explicitly() {
    let report = import_source(
        br"
        rtmp {
          server {
            listen 1935;
            application dormant {}
            application recorded {
              live on;
              record all;
              record_path /var/lib/recordings;
            }
          }
        }
        ",
        &[],
    );
    let config = report.config.expect("defaulted RTMP configuration");
    let dormant = &config.rtmp_services[0].applications[0];
    assert!(!dormant.live);
    assert!(dormant.idle_streams);
    assert!(dormant.recorders.is_empty());
    let recorder = &config.rtmp_services[0].applications[1].recorders[0];
    assert_eq!(recorder.start, RtmpRecorderStart::Continuous);
    assert_eq!(recorder.suffix_template, ".flv");
    assert!(!recorder.append_unix_seconds);
    assert_eq!(recorder.rotation_interval_ms, None);
}

#[test]
fn includes_are_transparent_and_parent_policy_after_the_include_is_inherited() {
    let report = import_source(
        br"
        rtmp {
          server {
            include applications.conf;
            listen 127.0.0.1:1935;
            live on;
            idle_streams off;
          }
        }
        ",
        &[(
            "applications.conf",
            br"application included { record all; record_path /var/lib/included; }",
        )],
    );

    let config = report.config.expect("included RTMP application");
    let application = &config.rtmp_services[0].applications[0];
    assert!(application.live);
    assert!(!application.idle_streams);
    let include = report
        .occurrence_ledger
        .iter()
        .find(|decision| decision.name.value == b"include")
        .expect("include decision");
    assert_eq!(include.disposition, OccurrenceDisposition::Structural);
    let application_decision = report
        .occurrence_ledger
        .iter()
        .find(|decision| decision.name.value == b"application")
        .expect("included application decision");
    assert_eq!(application_decision.provenance.include_stack.len(), 1);
}

#[test]
fn maps_only_nginx_manual_recording_that_also_selects_all_media() {
    let exact = import_source(
        br"rtmp { server { listen 1935; application app { live on; record all manual; record_path /var/lib/manual; } } }",
        &[],
    );
    assert_eq!(
        exact.config.expect("exact manual recorder").rtmp_services[0].applications[0].recorders[0]
            .start,
        RtmpRecorderStart::Manual
    );

    let bare = import_source(
        br"rtmp { server { listen 1935; application app { live on; record manual; record_path /var/lib/manual; } } }",
        &[],
    );
    assert!(bare.config.is_none());
    assert!(bare.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == E_SEMANTICS_NOT_REPRESENTABLE
            && diagnostic.message().contains("no nginx audio/video bits")
    }));
}

#[test]
fn enforces_exact_path_suffix_interval_and_listener_bounds() {
    let boundary_suffix = "x".repeat(128);
    let boundary_source = format!(
        "rtmp {{ server {{ listen [::1]:1935; application app {{ live on; record all; record_path /var//lib/recordings; record_suffix {boundary_suffix}; record_interval 2147483647; }} }} }}"
    );
    let maximum = import_source(boundary_source.as_bytes(), &[]);
    let recorder = &maximum
        .config
        .expect("maximum exact recorder values")
        .rtmp_services[0]
        .applications[0]
        .recorders[0];
    assert_eq!(recorder.root_directory, Path::new("/var/lib/recordings"));
    assert_eq!(recorder.suffix_template.len(), 128);
    assert_eq!(recorder.rotation_interval_ms, Some(2_147_483_647));

    for directive in [
        "record_path relative/path;",
        "record_path /var/../recordings;",
        "record_path /var/lib/recordings/;",
        "record_suffix bad/path.flv;",
        "record_suffix -%Q.flv;",
        "record_interval 0;",
        "record_interval 2147483648;",
    ] {
        let record_path = if directive.starts_with("record_path ") {
            ""
        } else {
            "record_path /var/lib/recordings;"
        };
        let source = format!(
            "rtmp {{ server {{ listen 1935; application app {{ live on; record all; {record_path} {directive} }} }} }}"
        );
        let report = import_source(source.as_bytes(), &[]);
        assert!(report.config.is_none(), "{directive}");
    }

    let oversized_suffix = "x".repeat(129);
    let source = format!(
        "rtmp {{ server {{ listen 1935; application app {{ live on; record all; record_path /var/lib/recordings; record_suffix {oversized_suffix}; }} }} }}"
    );
    assert!(import_source(source.as_bytes(), &[]).config.is_none());

    let listen_option = import_source(
        br"rtmp { server { listen 1935 bind; application app {} } }",
        &[],
    );
    assert!(listen_option.config.is_none());
    assert!(listen_option.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == E_UNSUPPORTED_FEATURE
            && diagnostic.message().contains("listen options")
    }));
}

#[test]
fn separate_entry_ignores_http_semantics_and_lowers_global_rtmp_auto_push() {
    let separate = import_source(
        br"http { server { listen 80; location / { proxy_pass http://backend; } } } rtmp { server { listen 1935; application app {} } }",
        &[],
    );
    assert!(separate.config.is_some());
    assert!(separate.occurrence_ledger.iter().any(|decision| {
        decision.name.value == b"proxy_pass"
            && decision.disposition == OccurrenceDisposition::Structural
    }));

    let global = import_source(
        br"rtmp_auto_push on; rtmp { server { listen 1935; application app {} } }",
        &[],
    );
    let config = global.config.expect("global auto-push policy");
    assert!(global.blocked_services.is_empty());
    assert!(config.rtmp_services[0].auto_push.enabled);
}

#[test]
fn duplicates_and_overlapping_listens_are_terminal_blockers() {
    for source in [
        br"rtmp { server { listen 1935; application app { live on; live off; } } }".as_slice(),
        br"rtmp { server { listen 0.0.0.0:1935; application one {} } server { listen 127.0.0.1:1935; application two {} } }".as_slice(),
        br"rtmp { server { listen 1935; application same {} application same {} } }".as_slice(),
    ] {
        let report = import_source(source, &[]);
        assert!(report.config.is_none());
        assert!(report.diagnostics.iter().any(|diagnostic| {
            diagnostic.code() == E_DUPLICATE_IDENTITY
                && diagnostic.stage() == DiagnosticStage::Resolve
        }));
    }
}

#[test]
fn lowers_extended_recorder_forms() {
    for (directive, audio, video, keyframes, append, lock, notify, max_size, max_frames) in [
        (
            "record audio;",
            true,
            false,
            false,
            false,
            false,
            false,
            None,
            None,
        ),
        (
            "record video;",
            false,
            true,
            false,
            false,
            false,
            false,
            None,
            None,
        ),
        (
            "record keyframes;",
            false,
            true,
            true,
            false,
            false,
            false,
            None,
            None,
        ),
        (
            "record_append on;",
            true,
            true,
            false,
            true,
            false,
            false,
            None,
            None,
        ),
        (
            "record_lock on;",
            true,
            true,
            false,
            false,
            true,
            false,
            None,
            None,
        ),
        (
            "record_notify on;",
            true,
            true,
            false,
            false,
            false,
            true,
            None,
            None,
        ),
        (
            "record_max_size 1m;",
            true,
            true,
            false,
            false,
            false,
            false,
            Some(1_048_576),
            None,
        ),
        (
            "record_max_frames 100;",
            true,
            true,
            false,
            false,
            false,
            false,
            None,
            Some(100),
        ),
    ] {
        let inherited_record = if directive.starts_with("record ") {
            ""
        } else {
            "record all;"
        };
        let source = format!(
            "rtmp {{ server {{ listen 1935; application app {{ live on; {inherited_record} record_path /var/lib/recordings; {directive} }} }} }}"
        );
        let report = import_source(source.as_bytes(), &[]);
        let recorder = &report.config.expect("extended recorder form").rtmp_services[0]
            .applications[0]
            .recorders[0];
        assert!(report.blocked_services.is_empty(), "{directive}");
        assert_eq!(recorder.record_mask.audio, audio, "{directive}");
        assert_eq!(recorder.record_mask.video, video, "{directive}");
        assert_eq!(recorder.record_mask.keyframes, keyframes, "{directive}");
        assert_eq!(recorder.append, append, "{directive}");
        assert_eq!(recorder.lock, lock, "{directive}");
        assert_eq!(recorder.notify, notify, "{directive}");
        assert_eq!(recorder.max_size, max_size, "{directive}");
        assert_eq!(recorder.max_frames, max_frames, "{directive}");
    }
}

#[test]
fn lowers_named_recorders_and_explicit_disabled_file_policies() {
    let report = import_source(
        br"
        rtmp {
          server {
            listen 1935;
            application app {
              live on;
              record off;
              recorder archive {
                record all;
                record_path /var/lib/archive;
                record_append off;
                record_lock off;
                record_max_size 0;
                record_max_frames 0;
                record_notify off;
              }
              recorder manual {
                record all manual;
                record_path /var/lib/manual;
              }
            }
          }
        }
        ",
        &[],
    );

    let config = report
        .config
        .as_ref()
        .expect("named recorder configuration");
    let recorders = &config.rtmp_services[0].applications[0].recorders;
    assert_eq!(recorders.len(), 2);
    assert_eq!(recorders[0].name, "archive");
    assert_eq!(recorders[0].start, RtmpRecorderStart::Continuous);
    assert_eq!(recorders[1].name, "manual");
    assert_eq!(recorders[1].start, RtmpRecorderStart::Manual);
    for path in [
        "/rtmp_services/0/applications/0/recorders/0/name",
        "/rtmp_services/0/applications/0/recorders/1/name",
    ] {
        assert!(report.provenance.iter().any(|entry| entry.path == path));
    }
}

#[test]
fn duplicate_named_recorders_are_blocking_even_when_recording_is_off() {
    let report = import_source(
        br"rtmp { server { listen 1935; application app { recorder archive { record off; } recorder archive { record off; } } } }",
        &[],
    );

    assert!(report.config.is_none());
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == E_DUPLICATE_IDENTITY && diagnostic.message().contains("recorder name")
    }));
}

#[test]
fn source_noop_hls_muxdelay_does_not_block_an_exact_application() {
    let report = import_source(
        br"rtmp { server { listen 1935; application app { hls_muxdelay 700ms; } } }",
        &[],
    );

    assert!(report.config.is_some());
    let decision = report
        .occurrence_ledger
        .iter()
        .find(|decision| decision.name.value == b"hls_muxdelay")
        .expect("hls_muxdelay decision");
    assert_eq!(decision.disposition, OccurrenceDisposition::Resolved);
}

#[test]
fn keeps_supported_servers_in_the_draft_without_placeholder_for_a_blocked_server() {
    let directory = fixture("phoenix-audited-partial.conf");
    let report =
        import_rtmp_with_timezone(Path::new("nginx.conf"), directory.path(), "America/Bahia");

    assert!(report.config.is_none());
    assert_eq!(report.blocked_services.len(), 1);
    assert_eq!(report.draft.rtmp_services.len(), 1);
    assert_eq!(report.draft.listeners.len(), 1);
    assert_eq!(report.draft.rtmp_services[0].applications[0].name, "safe");
    assert!(
        report
            .draft
            .rtmp_services
            .iter()
            .flat_map(|service| &service.applications)
            .all(|application| application.name != "phoenix")
    );
    assert_eq!(
        report.occurrence_ledger.len(),
        report.source_graph.expanded_occurrences.len()
    );
}

#[test]
fn resolve_entry_point_preserves_every_terminal_occurrence_decision() {
    let directory = TempDir::new().expect("RTMP semantic directory");
    fs::write(
        directory.path().join("nginx.conf"),
        b"http { server { listen 80; } } rtmp { server { listen 1935; application app { mystery on; } } }",
    )
    .expect("write semantic source");
    let graph = load(Path::new("nginx.conf"), directory.path());
    let occurrence_count = graph.value().expanded_occurrences.len();
    let resolved = resolve_rtmp(graph);

    assert_eq!(resolved.value().decisions.len(), occurrence_count);
    assert!(
        resolved
            .value()
            .decisions
            .iter()
            .enumerate()
            .all(|(index, decision)| { decision.occurrence.get() == index })
    );
    assert!(resolved.value().decisions.iter().any(|decision| {
        decision.name.value == b"mystery"
            && decision.disposition == OccurrenceDisposition::Blocking(E_UNSUPPORTED_FEATURE)
    }));
}

fn import_source(
    root: &[u8],
    includes: &[(&str, &[u8])],
) -> oxiroute_import::nginx::RtmpImportReport {
    let directory = TempDir::new().expect("RTMP source directory");
    fs::write(directory.path().join("nginx.conf"), root).expect("write RTMP root");
    for (name, contents) in includes {
        fs::write(directory.path().join(name), contents).expect("write RTMP include");
    }
    import_rtmp_with_timezone(Path::new("nginx.conf"), directory.path(), "America/Bahia")
}

fn fixture(name: &str) -> TempDir {
    let directory = TempDir::new().expect("RTMP fixture directory");
    fs::copy(
        Path::new("tests/fixtures/nginx").join(name),
        directory.path().join("nginx.conf"),
    )
    .expect("copy RTMP fixture");
    directory
}
