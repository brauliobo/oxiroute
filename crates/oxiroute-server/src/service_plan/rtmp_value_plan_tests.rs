use std::error::Error as _;

use oxiroute_config::ConfigDraft;

use crate::{
    planning_errors::rtmp_preparation_error, rtmp::value_plan::compile_rtmp_value_plans_from_draft,
};

use super::*;

#[test]
fn validated_config_translates_to_value_plans_without_acquisition() {
    let config = oxiroute_config_source::load_lua(
        r#"
return {
  version = 1,
  listeners = {},
  rtmp_services = {
    {
      name = "streaming",
      callbacks = { on_connect = "https://callback.example.test/connect" },
      auto_push = {
        enabled = true,
        socket_dir = "/not-created/auto-push",
        secret_file = "/not-read/auto-push-secret",
      },
      exec_profiles = {
        {
          name = "publisher",
          application = "live",
          executable = "/not-run/transcoder",
          arguments = { "--input", "$name" },
          environment = { { name = "TOKEN", value = "not-exposed" } },
          working_directory = "/not-read/work",
        },
      },
      applications = {
        {
          name = "live",
          live = true,
          publish = {
            rules = { { action = "allow", network = "2001:db8::/128" } },
            token = { source = "stream_query", parameter = "token", secret = "secret" },
          },
          push_targets = {
            {
              host = "unresolved.example.test",
              application = "$name",
              credentials = {
                username = "relay",
                secret_file = "/not-read/relay-secret",
              },
            },
          },
          callbacks = { on_publish = "https://callback.example.test/publish" },
          vod = {
            sources = {
              { type = "local", name = "archive", root_directory = "/not-read/vod" },
              { type = "http", name = "origin", origin = "https://media.example.test/library" },
            },
          },
          hls = { root_directory = "/not-created/hls" },
          dash = { root_directory = "/not-created/dash" },
          recorders = {
            { name = "archive", root_directory = "/not-created/recordings" },
          },
        },
      },
    },
  },
}
"#,
    )
    .expect("representative validated RTMP config");

    let plans = compile_rtmp_value_plans(&config).expect("opaque RTMP value plans");

    assert_eq!(plans.len(), 1);
    let plan = &plans[0];
    assert_eq!(plan.service_id(), "streaming");
    assert!(plan.auto_push().is_some());
    let application = &plan.applications()[0];
    assert!(application.media().is_some());
    assert!(application.vod().is_some());
    assert_eq!(application.recorders().len(), 1);
    assert_eq!(application.exec().len(), 1);
    assert_eq!(application.relay().push().len(), 1);
}

#[test]
fn canonical_minimum_and_maximum_bounds_translate_to_value_plans() {
    let minimum = oxiroute_config_source::load_lua(
        r#"
return {
  version = 1,
  listeners = {},
  rtmp_services = {
    {
      name = "minimum",
      outbound_chunk_size = 1,
      max_inbound_message_size = 1,
      ack_window_size = 1,
      applications = {
        {
          name = "live",
          limits = { max_connections = 1, max_publishers = 1, max_viewers = 1 },
          fanout = {
            max_subscribers = 1,
            max_queue_messages_per_subscriber = 1,
            max_queue_bytes_per_subscriber = 1,
          },
          relay = {
            max_queue_messages = 1,
            max_queue_bytes = 1,
            buffer_ms = 1,
            push_reconnect_ms = 1,
            pull_reconnect_ms = 1,
            dns_refresh_ms = 1000,
            connect_timeout_ms = 1,
            handshake_timeout_ms = 1,
          },
        },
      },
    },
  },
}
"#,
    )
    .expect("canonical RTMP minima");
    assert_eq!(compile_rtmp_value_plans(&minimum).unwrap().len(), 1);

    let draft = minimum.to_draft();
    let mut service = draft.rtmp_services[0].clone();
    service.name = "maximum".into();
    service.outbound_chunk_size = 1_048_576;
    service.max_inbound_message_size = 8_388_608;
    service.ack_window_size = u32::MAX;
    let application = &mut service.applications[0];
    application.limits.max_connections = 100_000;
    application.limits.max_publishers = 10_000;
    application.limits.max_viewers = 1_000_000;
    application.fanout.max_subscribers = 1_000_000;
    application.fanout.max_queue_messages_per_subscriber = 65_536;
    application.fanout.max_queue_bytes_per_subscriber = 1_073_741_824;
    application.relay.max_queue_messages = 65_536;
    application.relay.max_queue_bytes = 1_073_741_824;
    application.relay.buffer_ms = 60_000;
    application.relay.push_reconnect_ms = 300_000;
    application.relay.pull_reconnect_ms = 300_000;
    application.relay.dns_refresh_ms = 300_000;
    application.relay.connect_timeout_ms = 30_000;
    application.relay.handshake_timeout_ms = 30_000;
    let maximum = ConfigDraft {
        rtmp_services: vec![service],
        ..draft
    }
    .validate()
    .expect("canonical RTMP maxima");

    let plans = compile_rtmp_value_plans(&maximum).expect("maximum value plans");
    assert_eq!(plans[0].outbound_chunk_size(), 1_048_576);
    assert_eq!(
        plans[0].applications()[0].fanout().max_subscribers(),
        1_000_000
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn covered_canonical_policy_matrix_rejects_before_value_translation() {
    let base = oxiroute_config_source::load_lua(
        r#"
return {
  version = 1,
  listeners = {},
  rtmp_services = {
    {
      name = "streaming",
      applications = { { name = "live", live = true } },
    },
  },
}
"#,
    )
    .expect("canonical policy base")
    .to_draft();

    let mut invalid = Vec::new();

    let mut duplicate_services = base.clone();
    duplicate_services
        .rtmp_services
        .push(duplicate_services.rtmp_services[0].clone());
    invalid.push(duplicate_services);

    let mut duplicate_applications = base.clone();
    let application = duplicate_applications.rtmp_services[0].applications[0].clone();
    duplicate_applications.rtmp_services[0]
        .applications
        .push(application);
    invalid.push(duplicate_applications);

    let mut duplicate_push = base.clone();
    duplicate_push.rtmp_services[0].outbound_policy.deny_private = false;
    let target = oxiroute_config::RtmpPushTarget {
        host: "origin.example.test".into(),
        port: 1935,
        application: "$name".into(),
        scheme: oxiroute_config::RtmpTransport::Rtmp,
        stream_name: None,
        tc_url: None,
        flash_version: None,
        credentials: None,
    };
    duplicate_push.rtmp_services[0].applications[0].push_targets = vec![target.clone(), target];
    invalid.push(duplicate_push);

    let mut non_live_push = base.clone();
    non_live_push.rtmp_services[0].applications[0].live = false;
    non_live_push.rtmp_services[0].applications[0]
        .push_targets
        .push(oxiroute_config::RtmpPushTarget {
            host: "origin.example.test".into(),
            port: 1935,
            application: "$name".into(),
            scheme: oxiroute_config::RtmpTransport::Rtmp,
            stream_name: None,
            tc_url: None,
            flash_version: None,
            credentials: None,
        });
    invalid.push(non_live_push);

    let mut non_live_media = base.clone();
    non_live_media.rtmp_services[0].applications[0].live = false;
    non_live_media.rtmp_services[0].applications[0].hls =
        Some(serde_json::from_value(serde_json::json!({"root_directory": "/media"})).unwrap());
    invalid.push(non_live_media);

    let mut shared_root = base.clone();
    let first: oxiroute_config::RtmpHlsPolicy = serde_json::from_value(serde_json::json!({
        "root_directory": "/shared"
    }))
    .unwrap();
    let mut second = first.clone();
    second.max_storage_bytes += 1;
    shared_root.rtmp_services[0].applications[0].hls = Some(first);
    let mut second_application = shared_root.rtmp_services[0].applications[0].clone();
    second_application.name = "backup".into();
    second_application.hls = Some(second);
    shared_root.rtmp_services[0]
        .applications
        .push(second_application);
    invalid.push(shared_root);

    let mut unknown_exec_application = base.clone();
    unknown_exec_application.rtmp_services[0]
        .exec_profiles
        .push(
            serde_json::from_value(serde_json::json!({
                "name": "profile",
                "application": "missing",
                "executable": "/bin/transcoder",
                "working_directory": "/work"
            }))
            .unwrap(),
        );
    invalid.push(unknown_exec_application);

    for (index, draft) in invalid.into_iter().enumerate() {
        assert!(draft.validate().is_err(), "canonical policy case {index}");
    }

    let mut too_many_services = base;
    for index in 1..=64 {
        let mut service = too_many_services.rtmp_services[0].clone();
        service.name = format!("service-{index}");
        too_many_services.rtmp_services.push(service);
    }
    assert!(
        too_many_services.validate().is_err(),
        "canonical service count +1"
    );
}

#[test]
fn translator_errors_map_to_typed_redacted_service_plan_errors() {
    const SECRET_PATH: &str = "/secret/tenant/transcoder-token";
    const SECRET_TOKEN: &str = "token=super-secret";
    const SECRET_URL: &str = "https://user:password@example.test/private";
    let mut config = oxiroute_config_source::load_lua(
        r#"
return {
  version = 1,
  listeners = {},
  rtmp_services = {
    {
      name = "streaming",
      exec_profiles = {
        {
          name = "publisher",
          application = "live",
          executable = "/bin/transcoder",
          working_directory = "/work",
        },
      },
      applications = { { name = "live", live = true } },
    },
  },
}
"#,
    )
    .unwrap()
    .to_draft();
    config.rtmp_services[0].exec_profiles[0].executable = "/bin/sh".into();
    config.rtmp_services[0].exec_profiles[0].working_directory = SECRET_PATH.into();
    config.rtmp_services[0].exec_profiles[0].arguments = vec![SECRET_TOKEN.into()];
    config.rtmp_services[0].applications[0].callbacks.on_publish = Some(SECRET_URL.into());
    config.rtmp_services[0].applications[0].recorders.push(
        serde_json::from_value(serde_json::json!({
            "name": "archive",
            "root_directory": SECRET_PATH,
        }))
        .unwrap(),
    );

    let source = compile_rtmp_value_plans_from_draft(&config).unwrap_err();
    let error = rtmp_preparation_error(source);
    let ServicePlanError::RtmpPreparation(source) = &error else {
        panic!("typed RTMP preparation error")
    };
    assert_eq!(source.service_id(), Some("streaming"));
    assert_eq!(source.application_name(), Some("live"));
    assert_eq!(source.profile_name(), Some("publisher"));
    assert_eq!(source.recorder_name(), None);
    assert_eq!(source.field(), "exec.executable");
    assert_eq!(source.category(), oxiroute_rtmp::RtmpPrepareCategory::Value);
    assert!(error.source().is_some());
    assert!(error.source().unwrap().source().is_some());

    let rendered = format!("{error:#}");
    for expected in [
        "service `streaming`",
        "application `live`",
        "profile `publisher`",
        "exec.executable",
    ] {
        assert!(
            rendered.contains(expected),
            "missing {expected}: {rendered}"
        );
    }
    for secret in [SECRET_PATH, SECRET_TOKEN, SECRET_URL] {
        assert!(!rendered.contains(secret), "leaked {secret}: {rendered}");
        let mut current: Option<&(dyn std::error::Error + 'static)> = Some(&error);
        while let Some(source) = current {
            assert!(
                !source.to_string().contains(secret),
                "source leaked {secret}"
            );
            current = source.source();
        }
    }

    config.rtmp_services[0].exec_profiles.clear();
    config.rtmp_services[0].applications[0].callbacks.on_publish = None;
    config.rtmp_services[0].applications[0].recorders[0].root_directory = "relative-secret".into();
    let recorder = compile_rtmp_value_plans_from_draft(&config).unwrap_err();
    assert_eq!(recorder.service_id(), Some("streaming"));
    assert_eq!(recorder.application_name(), Some("live"));
    assert_eq!(recorder.recorder_name(), Some("archive"));
    assert_eq!(recorder.profile_name(), None);
    assert_eq!(recorder.field(), "recorder.root_directory");
    assert!(!recorder.to_string().contains("relative-secret"));
}
