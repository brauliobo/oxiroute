use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use tempfile::tempdir;

use oxiroute_rtmp::{
    DashSegmentNaming, ExecFilesystemPolicy, ExecLimits, ExecMode, ExecNetworkPolicy, ExecTrigger,
    HlsFragmentNaming, HlsKeyConfig, HlsVariant, MediaStoreLimits, RecorderMediaMask,
    RecorderWorkerConfig, RecordingPathPolicy, RecordingStoreLimits, RtmpAccessAction,
    RtmpAccessPlan, RtmpAccessRulePlan, RtmpApplicationPlan, RtmpAutoPushConfig, RtmpAutoPushPlan,
    RtmpCallbackError, RtmpCallbackEventPlan, RtmpCallbackMethod, RtmpCallbackPlan,
    RtmpCallbackPolicy, RtmpClientPlan, RtmpCredentialPlan, RtmpDashPlan, RtmpExecEnvironmentPlan,
    RtmpExecPlan, RtmpFanoutPlan, RtmpHlsPlan, RtmpMediaPlan, RtmpMediaStoreRegistry, RtmpNetwork,
    RtmpOutboundPolicy, RtmpPrepareCategory, RtmpPrepareContext, RtmpPrepareMode,
    RtmpPrepareSource, RtmpPullPlan, RtmpPushApplication, RtmpPushPlan, RtmpRecorderPlan,
    RtmpRecorderStart, RtmpRelayPlan, RtmpServicePlan, RtmpSessionCeilings, RtmpSessionLimits,
    RtmpTransport, RtmpVodPlan, VodLimits, VodSourceDefinition,
};

fn relay() -> RtmpRelayPlan {
    RtmpRelayPlan::new(
        RtmpOutboundPolicy::default(),
        8,
        4_096,
        Duration::from_secs(1),
        Duration::from_secs(1),
        Duration::from_secs(1),
        Duration::from_secs(30),
        Duration::from_secs(1),
        Duration::from_secs(1),
        [],
        [],
    )
    .expect("relay plan")
}

fn application(media: Option<RtmpMediaPlan>) -> RtmpApplicationPlan {
    RtmpApplicationPlan::new(
        "live",
        true,
        true,
        RtmpAccessPlan::default(),
        RtmpAccessPlan::default(),
        RtmpSessionCeilings::new(16, 4, 12),
        RtmpFanoutPlan::new(12, 8, 4_096).expect("fanout plan"),
        relay(),
        media,
        [],
        None,
        RtmpCallbackPlan::default(),
        [],
    )
    .expect("application plan")
}

fn plan(application: RtmpApplicationPlan, auto_push: Option<RtmpAutoPushPlan>) -> RtmpServicePlan {
    RtmpServicePlan::new(
        "streaming",
        4_096,
        RtmpSessionLimits::default(),
        RtmpCallbackPlan::default(),
        [application],
        auto_push,
    )
    .expect("service plan")
}

#[test]
fn plans_are_equal_and_deterministic_values() {
    let first = plan(application(None), None);
    let second = plan(application(None), None);

    assert_eq!(first, second);
    assert_eq!(format!("{first:?}"), format!("{second:?}"));
}

#[test]
fn context_normalizes_configured_candidate_listener_addresses_without_binding() {
    let first = "127.0.0.1:1935".parse::<SocketAddr>().unwrap();
    let second = "[::1]:1936".parse::<SocketAddr>().unwrap();
    let context = RtmpPrepareContext::new(RtmpPrepareMode::Validation, [second, first, second]);

    assert_eq!(context.mode(), RtmpPrepareMode::Validation);
    assert_eq!(context.candidate_listener_addresses(), &[first, second]);
}

#[test]
fn callback_endpoint_acquisition_stays_inside_the_rtmp_plan_owner() {
    let callbacks = RtmpCallbackPlan::default()
        .with_endpoint(
            RtmpCallbackEventPlan::Connect,
            "http://127.0.0.1:9/callback",
        )
        .expect("intrinsic callback URL");

    assert_eq!(
        callbacks
            .acquire_endpoint(
                RtmpCallbackEventPlan::Connect,
                &RtmpOutboundPolicy::default()
            )
            .expect_err("private callback destination must be denied"),
        RtmpCallbackError::AddressPolicy
    );
    assert!(
        callbacks
            .acquire_endpoint(
                RtmpCallbackEventPlan::Disconnect,
                &RtmpOutboundPolicy::default()
            )
            .expect("missing callback remains absent")
            .is_none()
    );
}

#[test]
fn vod_acquisition_stays_inside_the_rtmp_plan_owner() {
    let root = tempdir().expect("VOD root");
    let plan = RtmpVodPlan::new(
        VodLimits {
            max_sessions: 2,
            max_file_bytes: 1_024,
            max_duration: Duration::from_secs(10),
        },
        [VodSourceDefinition::Local {
            name: "media".into(),
            root_directory: root.path().to_owned(),
        }],
        RtmpOutboundPolicy::default(),
    )
    .expect("VOD plan");

    let application = plan.acquire("streaming", "vod").expect("VOD application");
    assert_eq!(application.service(), "streaming");
    assert_eq!(application.application(), "vod");
    assert_eq!(application.limits().max_sessions, 2);
}

#[test]
fn runtime_application_construction_stays_inside_the_rtmp_plan_owner() {
    let runtime = application(None).build_runtime_application(
        application(None).fanout().runtime_hub(),
        [],
        [],
        RtmpCallbackPolicy::default(),
        None,
        None,
        [],
        [],
    );

    assert_eq!(runtime.name(), "live");
    assert!(runtime.live());
    assert!(runtime.idle_streams());
    assert_eq!(
        runtime.session_limits(),
        RtmpSessionCeilings::new(16, 4, 12)
    );
}

#[test]
fn media_store_registry_validates_without_opening_and_shares_activation_roots() {
    let root = tempdir().expect("media root");
    let media_root = root.path().join("media");
    let limits = MediaStoreLimits {
        max_bytes: 1_024,
        max_files: 4,
        max_active_streams: 2,
        max_file_bytes: 512,
    };
    let mut registry = RtmpMediaStoreRegistry::default();

    assert!(
        registry
            .prepare(&media_root, limits, RtmpPrepareMode::Validation)
            .expect("media preflight")
            .is_none()
    );
    let first = registry
        .prepare(&media_root, limits, RtmpPrepareMode::Activation)
        .expect("media activation")
        .expect("opened store");
    let second = registry
        .prepare(&media_root, limits, RtmpPrepareMode::Activation)
        .expect("shared media activation")
        .expect("opened store");

    assert!(Arc::ptr_eq(&first, &second));
}

#[test]
fn canonical_duplicate_policy_is_not_reapplied_by_value_plans() {
    let duplicate = application(None);
    let plan = RtmpServicePlan::new(
        "streaming",
        4_096,
        RtmpSessionLimits::default(),
        RtmpCallbackPlan::default(),
        [duplicate.clone(), duplicate],
        None,
    )
    .expect("canonical duplicate policy belongs to validated config");
    assert_eq!(plan.applications().len(), 2);
}

#[test]
fn identity_errors_retain_context() {
    let error = RtmpApplicationPlan::new(
        "live/nested",
        true,
        true,
        RtmpAccessPlan::default(),
        RtmpAccessPlan::default(),
        RtmpSessionCeilings::new(1, 1, 1),
        RtmpFanoutPlan::new(1, 1, 1).unwrap(),
        relay(),
        None,
        [],
        None,
        RtmpCallbackPlan::default(),
        [],
    )
    .unwrap_err();
    assert_eq!(error.category(), RtmpPrepareCategory::Identity);
    assert_eq!(error.field(), "application.name");
}

#[test]
fn malformed_intrinsic_values_are_rejected() {
    for network in [
        RtmpNetwork::Cidr {
            address: IpAddr::V4(Ipv4Addr::LOCALHOST),
            prefix: 33,
        },
        RtmpNetwork::Cidr {
            address: IpAddr::V6(Ipv6Addr::LOCALHOST),
            prefix: 129,
        },
    ] {
        let error = RtmpAccessRulePlan::new(RtmpAccessAction::Allow, network).unwrap_err();
        assert_eq!(error.category(), RtmpPrepareCategory::Value);
        assert_eq!(error.field(), "access.network");
    }

    for origin in [
        "http://user@example.invalid/media",
        "https://example.invalid/media?token=secret",
        "https://example.invalid/a/../media",
        "https://example.invalid/media%2Fclip",
        "ftp://example.invalid/media",
    ] {
        let error = RtmpVodPlan::new(
            VodLimits {
                max_sessions: 1,
                max_file_bytes: 1,
                max_duration: Duration::from_millis(1),
            },
            [VodSourceDefinition::Http {
                name: "origin".into(),
                origin: origin.into(),
            }],
            RtmpOutboundPolicy::default(),
        )
        .unwrap_err();
        assert_eq!(error.category(), RtmpPrepareCategory::Value, "{origin}");
        assert_eq!(error.field(), "vod.sources", "{origin}");
    }
}

#[test]
fn runtime_boundary_values_remain_representable() {
    for network in [
        RtmpNetwork::Cidr {
            address: IpAddr::V4(Ipv4Addr::LOCALHOST),
            prefix: 32,
        },
        RtmpNetwork::Cidr {
            address: IpAddr::V6(Ipv6Addr::LOCALHOST),
            prefix: 128,
        },
    ] {
        RtmpAccessRulePlan::new(RtmpAccessAction::Allow, network).expect("maximum CIDR prefix");
    }

    RtmpVodPlan::new(
        VodLimits {
            max_sessions: usize::MAX,
            max_file_bytes: u64::MAX,
            max_duration: Duration::MAX,
        },
        [VodSourceDefinition::Http {
            name: "origin".into(),
            origin: "https://example.invalid:443/media".into(),
        }],
        RtmpOutboundPolicy::default(),
    )
    .expect("runtime-representable VOD bounds and origin");
}

#[test]
#[allow(clippy::too_many_lines)]
fn covered_intrinsic_contract_matrix_rejects_invalid_and_accepts_boundaries() {
    assert!(
        RtmpCallbackPlan::new(RtmpCallbackMethod::Post, Duration::ZERO, Duration::ZERO).is_err()
    );
    RtmpCallbackPlan::new(
        RtmpCallbackMethod::Post,
        Duration::from_millis(1),
        Duration::ZERO,
    )
    .expect("zero callback update timeout is representable");

    let long_name = "a".repeat(513);
    assert!(
        RtmpApplicationPlan::new(
            long_name,
            true,
            true,
            RtmpAccessPlan::default(),
            RtmpAccessPlan::default(),
            RtmpSessionCeilings::new(1, 1, 1),
            RtmpFanoutPlan::new(1, 1, 1).unwrap(),
            relay(),
            None,
            [],
            None,
            RtmpCallbackPlan::default(),
            [],
        )
        .is_err()
    );
    assert!(
        RtmpAccessRulePlan::new(
            RtmpAccessAction::Allow,
            RtmpNetwork::Cidr {
                address: IpAddr::V4(Ipv4Addr::LOCALHOST),
                prefix: 33,
            },
        )
        .is_err()
    );
    assert!(oxiroute_rtmp::RtmpTokenPlan::new("", b"secret").is_err());
    assert!(oxiroute_rtmp::RtmpTokenPlan::new("token", b"").is_err());

    for variant in [
        HlsVariant {
            name: String::new(),
            bandwidth: 1,
            codecs: None,
            width: None,
            height: None,
        },
        HlsVariant {
            name: "main".into(),
            bandwidth: 0,
            codecs: None,
            width: None,
            height: None,
        },
        HlsVariant {
            name: "main".into(),
            bandwidth: 1,
            codecs: None,
            width: Some(1),
            height: None,
        },
    ] {
        assert!(
            RtmpHlsPlan::new(
                "/media".into(),
                Duration::from_millis(1),
                Duration::from_millis(1),
                Duration::from_millis(1),
                HlsFragmentNaming::Sequential,
                false,
                true,
                [variant],
                None,
                1,
                1,
                1,
                1,
                1,
            )
            .is_err()
        );
    }
    for keys in [
        HlsKeyConfig {
            rotation_segments: 0,
            url_prefix: String::new(),
        },
        HlsKeyConfig {
            rotation_segments: 1,
            url_prefix: "/absolute/".into(),
        },
    ] {
        assert!(
            RtmpHlsPlan::new(
                "/media".into(),
                Duration::from_millis(1),
                Duration::from_millis(1),
                Duration::from_millis(1),
                HlsFragmentNaming::Sequential,
                false,
                true,
                [],
                Some(keys),
                1,
                1,
                1,
                1,
                1,
            )
            .is_err()
        );
    }
    assert!(
        RtmpDashPlan::new(
            "/dash".into(),
            Duration::from_millis(2),
            Duration::from_millis(1),
            Duration::from_millis(2),
            DashSegmentNaming::Sequential,
            false,
            true,
            1,
            1,
            1,
            1,
            1,
        )
        .is_err()
    );

    let mut auto_push = RtmpAutoPushConfig {
        enabled: true,
        socket_dir: "/auto-push".into(),
        secret_file: None,
        reconnect_interval: Duration::from_millis(1),
        connect_timeout: Duration::from_millis(1),
        handshake_timeout: Duration::from_millis(1),
        max_peers: 1,
        max_queue_messages: 1,
        max_queue_bytes: 1,
        max_streams: 1,
    };
    auto_push.max_peers = 0;
    assert!(RtmpAutoPushPlan::new(auto_push).is_err());

    for limits in [
        VodLimits {
            max_sessions: 0,
            max_file_bytes: 1,
            max_duration: Duration::from_millis(1),
        },
        VodLimits {
            max_sessions: 1,
            max_file_bytes: 0,
            max_duration: Duration::from_millis(1),
        },
        VodLimits {
            max_sessions: 1,
            max_file_bytes: 1,
            max_duration: Duration::ZERO,
        },
    ] {
        assert!(RtmpVodPlan::new(limits, [], RtmpOutboundPolicy::default()).is_err());
    }

    let path_policy = RecordingPathPolicy::new(".flv", false).unwrap();
    let worker = RecorderWorkerConfig {
        record_mask: RecorderMediaMask::new(false, false, false),
        ..RecorderWorkerConfig::default()
    };
    let error = RtmpRecorderPlan::new(
        "archive",
        RtmpRecorderStart::Continuous,
        "/recordings".into(),
        path_policy.clone(),
        worker,
        RecordingStoreLimits {
            max_bytes: None,
            max_files: None,
            max_active_recorders: 1,
        },
    )
    .unwrap_err();
    assert_eq!(error.recorder_name(), Some("archive"));
    let worker = RecorderWorkerConfig {
        rotation_interval: Some(Duration::ZERO),
        ..RecorderWorkerConfig::default()
    };
    assert!(
        RtmpRecorderPlan::new(
            "archive",
            RtmpRecorderStart::Continuous,
            "/recordings".into(),
            path_policy,
            worker,
            RecordingStoreLimits {
                max_bytes: None,
                max_files: None,
                max_active_recorders: 0,
            },
        )
        .is_err()
    );

    let valid_limits = ExecLimits::new(
        1,
        1,
        1,
        1,
        Duration::from_millis(1),
        Duration::from_millis(1),
        1,
        Duration::from_millis(1),
        0,
    )
    .unwrap();
    assert!(RtmpExecEnvironmentPlan::new("BAD=NAME", "value").is_err());
    let error = RtmpExecPlan::new(
        "profile",
        "live",
        ExecMode::Command,
        ExecTrigger::Publisher,
        "/bin/sh".into(),
        [],
        [],
        "/work".into(),
        ExecFilesystemPolicy::WorkingDirectory,
        ExecNetworkPolicy::Disabled,
        valid_limits,
        false,
    )
    .unwrap_err();
    assert_eq!(error.profile_name(), Some("profile"));

    assert!(RtmpCredentialPlan::new("", "/secret".into()).is_err());
    assert!(RtmpCredentialPlan::new("user", "relative".into()).is_err());
    assert!(RtmpClientPlan::new("", 0, None, None).is_err());
    assert!(
        RtmpClientPlan::new(
            "client",
            0,
            Some("http://example.invalid/live".into()),
            None,
        )
        .is_err()
    );
    let client = RtmpClientPlan::new("client", u32::MAX, None, None).unwrap();
    assert!(
        RtmpPushPlan::new(
            "",
            1935,
            RtmpTransport::Rtmp,
            RtmpPushApplication::StreamName,
            None,
            client.clone(),
        )
        .is_err()
    );
    assert!(
        RtmpPullPlan::new(
            "origin.example",
            0,
            RtmpTransport::Rtmp,
            "live",
            "stream",
            "local",
            "stream",
            client,
        )
        .is_err()
    );

    for field in ["allow", "deny"] {
        let mut policy = RtmpOutboundPolicy::default();
        if field == "allow" {
            policy.allow_cidrs = vec!["192.0.2.0/33".into()];
        } else {
            policy.deny_cidrs = vec!["2001:db8::/129".into()];
        }
        let error = RtmpRelayPlan::new(
            policy,
            1,
            1,
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_millis(1),
            [],
            [],
        )
        .unwrap_err();
        assert_eq!(error.field(), "relay.policy.cidrs");
        assert!(matches!(
            error.prepare_source(),
            Some(RtmpPrepareSource::OutboundPolicy(_))
        ));
    }
}

#[test]
fn owner_validation_errors_remain_typed_sources() {
    let limits = RtmpSessionLimits {
        max_inbound_message_size: 0,
        ..RtmpSessionLimits::default()
    };
    let error = RtmpServicePlan::new(
        "streaming",
        4_096,
        limits,
        RtmpCallbackPlan::default(),
        [application(None)],
        None,
    )
    .unwrap_err();

    assert_eq!(error.category(), RtmpPrepareCategory::Bound);
    assert!(matches!(
        error.prepare_source(),
        Some(RtmpPrepareSource::Session(_))
    ));
}

#[test]
fn construction_retains_paths_without_creating_or_reading_them() {
    let socket_dir = PathBuf::from("/not-created/auto-push");
    let secret_file = PathBuf::from("/not-read/auto-push-secret");
    let media_root = PathBuf::from("/not-created/media");
    let auto_push = RtmpAutoPushPlan::new(RtmpAutoPushConfig {
        enabled: true,
        socket_dir: socket_dir.clone(),
        secret_file: Some(secret_file.clone()),
        reconnect_interval: Duration::from_secs(1),
        connect_timeout: Duration::from_secs(1),
        handshake_timeout: Duration::from_secs(1),
        max_peers: 2,
        max_queue_messages: 8,
        max_queue_bytes: 4_096,
        max_streams: 4,
    })
    .expect("auto-push plan");
    let hls = RtmpHlsPlan::new(
        media_root,
        Duration::from_secs(1),
        Duration::from_secs(2),
        Duration::from_secs(10),
        HlsFragmentNaming::Sequential,
        false,
        true,
        [],
        None,
        1_024,
        8,
        8_192,
        16,
        2,
    )
    .expect("HLS plan");
    let media = RtmpMediaPlan::new(Some(hls), None).expect("media plan");

    let built = plan(application(Some(media)), Some(auto_push));

    assert_eq!(built.service_id(), "streaming");
    assert_eq!(built.auto_push().unwrap().config().socket_dir, socket_dir);
    assert_eq!(
        built.auto_push().unwrap().config().secret_file.as_ref(),
        Some(&secret_file)
    );
}

#[test]
fn debug_output_redacts_urls_and_all_filesystem_roots() {
    let callback = "https://example.invalid/private?token=secret";
    let callbacks = RtmpCallbackPlan::default()
        .with_endpoint(RtmpCallbackEventPlan::Publish, callback)
        .expect("callback plan");
    let auto_push = RtmpAutoPushPlan::new(RtmpAutoPushConfig {
        enabled: true,
        socket_dir: PathBuf::from("/private/auto-push-socket"),
        secret_file: Some(PathBuf::from("/private/auto-push-secret")),
        reconnect_interval: Duration::from_secs(1),
        connect_timeout: Duration::from_secs(1),
        handshake_timeout: Duration::from_secs(1),
        max_peers: 2,
        max_queue_messages: 8,
        max_queue_bytes: 4_096,
        max_streams: 4,
    })
    .unwrap();
    let hls = RtmpHlsPlan::new(
        PathBuf::from("/private/media-root"),
        Duration::from_secs(1),
        Duration::from_secs(2),
        Duration::from_secs(10),
        HlsFragmentNaming::Sequential,
        false,
        true,
        [],
        None,
        1_024,
        8,
        8_192,
        16,
        2,
    )
    .unwrap();

    let debug = format!("{callbacks:?} {auto_push:?} {hls:?}");
    for sensitive in [
        callback,
        "/private/auto-push-socket",
        "/private/auto-push-secret",
        "/private/media-root",
    ] {
        assert!(!debug.contains(sensitive));
    }
    assert!(debug.contains("<redacted>"));
}
