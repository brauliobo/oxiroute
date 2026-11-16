use std::{fs, path::Path};

use oxiroute_config::{Protocol, RtmpRecorderStart};
use oxiroute_import::{
    E_DUPLICATE_IDENTITY, E_SEMANTICS_NOT_REPRESENTABLE, E_UNSUPPORTED_FEATURE,
    nginx::{OccurrenceDisposition, import_rtmp},
};
use tempfile::TempDir;

use super::support::import_source;

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
            application live {}
          }
        }
        ",
        &[],
    );

    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("exact RTMP configuration");
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
    assert_eq!(application.name, "live");
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
fn recording_import_requires_an_explicit_host_iana_timezone() {
    let directory = TempDir::new().expect("RTMP source directory");
    fs::write(
        directory.path().join("nginx.conf"),
        b"rtmp { server { listen 127.0.0.1:1935; application live { live on; record all; record_path /srv/recordings; } } }",
    )
    .expect("write RTMP source");

    let report = import_rtmp(Path::new("nginx.conf"), directory.path());

    assert!(
        report
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
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
    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("defaulted RTMP configuration");
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
fn maps_only_nginx_manual_recording_that_also_selects_all_media() {
    let exact = import_source(
        br"rtmp { server { listen 1935; application app { live on; record all manual; record_path /var/lib/manual; } } }",
        &[],
    );
    assert_eq!(
        exact
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .expect("exact manual recorder")
            .rtmp_services[0]
            .applications[0]
            .recorders[0]
            .start,
        RtmpRecorderStart::Manual
    );

    let bare = import_source(
        br"rtmp { server { listen 1935; application app { live on; record manual; record_path /var/lib/manual; } } }",
        &[],
    );
    assert!(
        bare.validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
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
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
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
        assert!(
            report
                .validated()
                .map(oxiroute_config::ValidatedConfig::as_draft)
                .is_none(),
            "{directive}"
        );
    }

    let oversized_suffix = "x".repeat(129);
    let source = format!(
        "rtmp {{ server {{ listen 1935; application app {{ live on; record all; record_path /var/lib/recordings; record_suffix {oversized_suffix}; }} }} }}"
    );
    assert!(
        import_source(source.as_bytes(), &[])
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );

    let listen_option = import_source(
        br"rtmp { server { listen 1935 bind; application app {} } }",
        &[],
    );
    assert!(
        listen_option
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert!(listen_option.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == E_UNSUPPORTED_FEATURE
            && diagnostic.message().contains("listen options")
    }));
}

#[test]
#[allow(clippy::too_many_lines)]
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
        let recorder = &report
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .expect("extended recorder form")
            .rtmp_services[0]
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
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
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

    assert!(
        report
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == E_DUPLICATE_IDENTITY && diagnostic.message().contains("recorder name")
    }));
}
