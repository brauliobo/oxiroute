use std::{
    fs,
    sync::Arc,
    time::{Duration, Instant},
};

use oxiroute_config::{
    AccessLogPolicy, RtmpApplication, RtmpPushTarget, RtmpRecorderStart, RtmpService,
};
use oxiroute_rtmp::{PreparedRtmpRuntimeSet, RtmpPrepareContext, RtmpPrepareMode};
use oxiroute_server::{ServiceKind, ServicePlanError};
use tempfile::TempDir;

use crate::canonical::canonical_config;
use crate::config_support::{
    loopback_bind as socket_bind, rtmp_recorder as recorder, runtime_plan, service_specs,
};

#[test]
fn rtmp_listeners_share_one_service_identity_catalog_and_hub() {
    let mut config = canonical_config();
    let mut second_listener = config.listeners[3].clone();
    second_listener.name = "live-backup".into();
    second_listener.bind = socket_bind(1936);
    config.listeners.push(second_listener);

    let services =
        service_specs(&config.clone().validate().unwrap()).expect("valid shared RTMP service plan");
    let ServiceKind::Rtmp(first_plan) = &services[3].kind else {
        panic!("first RTMP listener must use the RTMP service");
    };
    let ServiceKind::Rtmp(second_plan) = &services[4].kind else {
        panic!("second RTMP listener must use the RTMP service");
    };
    assert!(Arc::ptr_eq(first_plan, second_plan));

    let context = RtmpPrepareContext::new(
        RtmpPrepareMode::Activation,
        [
            "127.0.0.1:1935".parse().unwrap(),
            "127.0.0.1:1936".parse().unwrap(),
        ],
    );
    let runtimes = PreparedRtmpRuntimeSet::prepare(
        [first_plan.value_plan()],
        &context,
        Instant::now() + Duration::from_secs(1),
    )
    .expect("prepared shared RTMP service")
    .start(Instant::now() + Duration::from_secs(1))
    .expect("started shared RTMP service");
    assert_eq!(
        runtimes.service("live").expect("RTMP service").service_id(),
        "live"
    );
}

#[test]
fn compiles_chunk_disabled_access_log_and_application_fanout_policy() {
    let mut config = canonical_config();
    let service = &mut config.rtmp_services[0];
    service.outbound_chunk_size = 8_192;
    service.access_log = Some(AccessLogPolicy::Disabled);
    service.applications[0].fanout = oxiroute_config::RtmpFanoutPolicy {
        max_subscribers: 7,
        max_queue_messages_per_subscriber: 11,
        max_queue_bytes_per_subscriber: 4_096,
    };

    let services =
        service_specs(&config.clone().validate().unwrap()).expect("lowered RTMP runtime policy");
    let ServiceKind::Rtmp(plan) = &services[3].kind else {
        panic!("RTMP service plan");
    };
    let fanout = plan.value_plan().applications()[0].fanout();
    assert_eq!(fanout.max_subscribers(), 7);
    assert_eq!(fanout.max_queue_messages_per_subscriber(), 11);
    assert_eq!(fanout.max_queue_bytes_per_subscriber(), 4_096);
}

#[test]
fn resolves_absent_push_port_without_connecting_and_rejects_direct_listener_loops() {
    let mut config = canonical_config();
    config.rtmp_services[0].outbound_policy.deny_private = false;
    config.rtmp_services[0].applications[0]
        .push_targets
        .push(RtmpPushTarget {
            host: "127.0.0.1".into(),
            port: 1_936,
            application: "$name".into(),
            scheme: oxiroute_config::RtmpTransport::Rtmp,
            stream_name: None,
            tc_url: None,
            flash_version: None,
            credentials: None,
        });
    service_specs(&config.clone().validate().unwrap())
        .expect("absent push destination is a runtime concern");

    config.rtmp_services[0].applications[0].push_targets[0].port = 1_935;
    assert!(matches!(
        service_specs(&config.clone().validate().unwrap()),
        Err(ServicePlanError::RtmpRuntimePreparation(_))
    ));
}

#[cfg(unix)]
#[test]
fn recorder_planning_is_read_only_and_runtime_activation_opens_the_store() {
    let root = TempDir::new().expect("recording root");
    let mut config = canonical_config();
    config.rtmp_services[0].applications[0]
        .recorders
        .push(recorder(
            "archive",
            RtmpRecorderStart::Continuous,
            root.path(),
        ));
    config.rtmp_services[0].applications[0]
        .recorders
        .push(recorder("clips", RtmpRecorderStart::Manual, root.path()));

    let plan = runtime_plan(&config.clone().validate().unwrap()).expect("recorder runtime plan");
    let services = service_specs(&config.clone().validate().unwrap()).expect("recorder services");

    let listener = plan
        .topology
        .nodes()
        .iter()
        .find(|node| node.kind == oxiroute_server::TopologyNodeKind::RtmpListener)
        .expect("RTMP topology listener");
    assert_eq!(
        listener.attributes["applications"][0]["recording"]["recorderCount"],
        2
    );
    assert!(plan.rtmp_recording_supported);
    assert!(
        fs::read_dir(root.path())
            .expect("root entries")
            .next()
            .is_none()
    );
    assert!(!root.path().join(".oxiroute-recording.lock").exists());
    let ServiceKind::Rtmp(service) = &services[3].kind else {
        panic!("RTMP service plan");
    };
    let context = RtmpPrepareContext::new(RtmpPrepareMode::Activation, []);
    let _runtime = PreparedRtmpRuntimeSet::prepare(
        [service.value_plan()],
        &context,
        Instant::now() + Duration::from_secs(1),
    )
    .expect("prepared RTMP runtime")
    .start(Instant::now() + Duration::from_secs(1))
    .expect("activated RTMP runtime");
    assert!(!root.path().join(".oxiroute-recording.lock").exists());
}

#[cfg(unix)]
#[test]
fn derives_manual_capability_from_canonical_recorder_policies() {
    let root = TempDir::new().expect("recording root");
    let mut continuous = canonical_config();
    continuous.rtmp_services[0].applications[0]
        .recorders
        .push(recorder(
            "archive",
            RtmpRecorderStart::Continuous,
            root.path(),
        ));
    let continuous_plan =
        runtime_plan(&continuous.clone().validate().unwrap()).expect("continuous recorder plan");
    assert!(continuous_plan.rtmp_capabilities.live_ingest);
    assert!(!continuous_plan.rtmp_capabilities.manual_recording);

    let mut manual = continuous;
    manual.rtmp_services[0].applications[0].recorders[0].start = RtmpRecorderStart::Manual;
    let manual_plan = runtime_plan(&manual.validate().unwrap()).expect("manual recorder plan");
    assert!(manual_plan.rtmp_capabilities.live_ingest);
    assert!(manual_plan.rtmp_capabilities.manual_recording);
}

#[cfg(unix)]
#[test]
fn excludes_unreferenced_rtmp_services_from_active_capabilities() {
    let root = TempDir::new().expect("recording root");
    let mut config = canonical_config();
    config.rtmp_services.push(RtmpService {
        name: "orphan".into(),
        outbound_chunk_size: 4_096,
        max_inbound_message_size: 8 * 1024 * 1024,
        ack_window_size: 5_000_000,
        access_log: None,
        outbound_policy: oxiroute_config::RtmpOutboundPolicy::default(),
        callbacks: oxiroute_config::RtmpCallbackConfig::default(),
        auto_push: oxiroute_config::RtmpAutoPushPolicy::default(),
        exec_profiles: Vec::new(),
        applications: vec![RtmpApplication {
            name: "unused".into(),
            live: true,
            idle_streams: false,
            publish: oxiroute_config::RtmpAccessPolicy::default(),
            play: oxiroute_config::RtmpAccessPolicy::default(),
            limits: oxiroute_config::RtmpSessionCeilings::default(),
            push_targets: Vec::new(),
            pull_targets: Vec::new(),
            relay: oxiroute_config::RtmpRelayPolicy::default(),
            callbacks: oxiroute_config::RtmpCallbackConfig::default(),
            fanout: oxiroute_config::RtmpFanoutPolicy::default(),
            vod: None,
            hls: None,
            dash: None,
            recorders: vec![recorder(
                "manual-orphan",
                RtmpRecorderStart::Manual,
                root.path(),
            )],
        }],
    });

    let plan =
        runtime_plan(&config.validate().unwrap()).expect("runtime plan with orphan RTMP service");

    assert!(plan.rtmp_capabilities.live_ingest);
    assert!(!plan.rtmp_capabilities.manual_recording);
    assert!(!plan.rtmp_recording_supported);
}

#[cfg(unix)]
#[test]
fn rejects_insecure_and_overquota_recording_roots_without_path_disclosure() {
    use std::os::unix::fs::PermissionsExt as _;

    let insecure_root = TempDir::new().expect("insecure recording root");
    fs::set_permissions(insecure_root.path(), fs::Permissions::from_mode(0o777))
        .expect("insecure root mode");
    let mut insecure = canonical_config();
    insecure.rtmp_services[0].applications[0]
        .recorders
        .push(recorder(
            "archive",
            RtmpRecorderStart::Continuous,
            insecure_root.path(),
        ));
    let Err(error) = runtime_plan(&insecure.validate().unwrap()) else {
        panic!("insecure root must fail")
    };
    assert!(matches!(error, ServicePlanError::RtmpRuntimePreparation(_)));
    assert!(
        !error
            .to_string()
            .contains(&insecure_root.path().display().to_string())
    );

    let overquota_root = TempDir::new().expect("overquota recording root");
    fs::write(overquota_root.path().join("existing.flv"), b"over quota")
        .expect("overquota fixture");
    let mut overquota = canonical_config();
    let mut policy = recorder(
        "archive",
        RtmpRecorderStart::Continuous,
        overquota_root.path(),
    );
    policy.max_queue_bytes = 1;
    policy.max_storage_bytes = Some(1);
    overquota.rtmp_services[0].applications[0]
        .recorders
        .push(policy);
    let Err(error) = runtime_plan(&overquota.validate().unwrap()) else {
        panic!("overquota root must fail")
    };
    assert!(matches!(error, ServicePlanError::RtmpRuntimePreparation(_)));
    assert!(
        !error
            .to_string()
            .contains(&overquota_root.path().display().to_string())
    );
}
