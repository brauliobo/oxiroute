use std::{ffi::OsString, os::unix::ffi::OsStringExt as _, path::PathBuf};

use super::{
    system::{
        MemoryInfo, ProcessStatus, parse_load_average, parse_memory_info, parse_process_stat,
        parse_process_status, parse_system_stat,
    },
    *,
};
use crate::listener::inventory::{ListenerDescriptorKind, ListenerDescriptorRole, ListenerPlane};

fn inventory_entry(id: ListenerId, name: &str, bind: ListenerBind) -> ListenerEntry {
    ListenerEntry {
        id: id.clone(),
        name: name.into(),
        protocol: oxiroute_config::Protocol::Http,
        bind,
        plane: ListenerPlane::Data,
        descriptor_role: match id {
            ListenerId::Traffic(name) | ListenerId::Legacy(name) => {
                ListenerDescriptorRole::Traffic(name)
            }
            ListenerId::Management => ListenerDescriptorRole::Management,
            ListenerId::Stats(index) => ListenerDescriptorRole::Stats(index),
            ListenerId::StatsPage(index) => ListenerDescriptorRole::StatsPage(index),
        },
        descriptor_kind: ListenerDescriptorKind::Tcp,
        metric_policy: ListenerMetricPolicy::Public,
        max_connections: None,
    }
}

#[test]
fn inventory_batch_failure_leaves_generation_and_process_registries_empty() {
    let process = ProcessRuntime::new(None);
    let metrics = RuntimeMetrics::for_process(process.clone());
    let entries = vec![
        inventory_entry(
            ListenerId::Traffic("valid".into()),
            "valid",
            ListenerBind::Socket {
                address: "127.0.0.1:8080".parse().expect("valid address"),
                ipv6_only: false,
            },
        ),
        inventory_entry(
            ListenerId::Traffic("invalid".into()),
            "invalid",
            ListenerBind::Unix {
                path: PathBuf::from(OsString::from_vec(vec![0xff])),
                mode: None,
            },
        ),
    ];

    assert!(matches!(
        metrics.register_inventory(&entries),
        Err(MetricsError::InvalidListenerBind { .. })
    ));
    assert!(metrics.internal_listener_snapshots().unwrap().is_empty());
    assert_eq!(process.listener_count(), 0);
}

#[test]
fn typed_listener_counters_are_reused_across_generations_without_sharing_lifecycle() {
    let process = ProcessRuntime::new(None);
    let entry = inventory_entry(
        ListenerId::Traffic("edge".into()),
        "edge",
        ListenerBind::Socket {
            address: "127.0.0.1:8080".parse().expect("edge address"),
            ipv6_only: false,
        },
    );
    let first = RuntimeMetrics::for_process(process.clone());
    first
        .register_inventory(std::slice::from_ref(&entry))
        .unwrap()
        .commit()
        .unwrap();
    let first_listener = first
        .inventory_listener(&entry.id)
        .unwrap()
        .expect("first listener");
    first_listener.mark_listening();
    drop(first_listener.begin_connection().expect("first connection"));

    let second = RuntimeMetrics::for_process(process.clone());
    second
        .register_inventory(std::slice::from_ref(&entry))
        .unwrap()
        .commit()
        .unwrap();
    let second_snapshot = second.internal_listener_snapshots().unwrap();

    assert_eq!(second_snapshot[0].accepted_connections, 1);
    assert_eq!(second_snapshot[0].state, ListenerRuntimeState::Configured);
    assert_eq!(
        first.internal_listener_snapshots().unwrap()[0].state,
        ListenerRuntimeState::Listening
    );
    assert_eq!(process.listener_count(), 1);
}

#[test]
fn failed_registration_cannot_remove_reused_or_concurrently_committed_process_entries() {
    let process = ProcessRuntime::new(None);
    let entry = inventory_entry(
        ListenerId::Traffic("edge".into()),
        "edge",
        ListenerBind::Socket {
            address: "127.0.0.1:8080".parse().expect("edge address"),
            ipv6_only: false,
        },
    );
    let first = RuntimeMetrics::for_process(process.clone());
    let failed = first
        .register_inventory(std::slice::from_ref(&entry))
        .expect("first reservation");
    let second = RuntimeMetrics::for_process(process.clone());
    second
        .register_inventory(std::slice::from_ref(&entry))
        .expect("concurrent reservation")
        .commit()
        .expect("concurrent commit");

    drop(failed);

    assert_eq!(process.listener_count(), 1);
    let third = RuntimeMetrics::for_process(process.clone());
    third
        .register_inventory(std::slice::from_ref(&entry))
        .expect("reuse committed entry")
        .commit()
        .expect("commit reused entry");
    assert_eq!(process.listener_count(), 1);
}

#[test]
fn public_snapshots_sort_by_display_name_while_internal_snapshots_keep_inventory_order() {
    let process = ProcessRuntime::new(None);
    let metrics = RuntimeMetrics::for_process(process);
    let mut entries = vec![
        inventory_entry(
            ListenerId::Traffic("zulu".into()),
            "zulu",
            ListenerBind::Socket {
                address: "127.0.0.1:8080".parse().unwrap(),
                ipv6_only: false,
            },
        ),
        inventory_entry(
            ListenerId::Traffic("alpha".into()),
            "alpha",
            ListenerBind::Socket {
                address: "127.0.0.1:8081".parse().unwrap(),
                ipv6_only: false,
            },
        ),
        inventory_entry(
            ListenerId::Management,
            "@management",
            ListenerBind::Socket {
                address: "127.0.0.1:9900".parse().unwrap(),
                ipv6_only: false,
            },
        ),
        inventory_entry(
            ListenerId::Stats(0),
            "@stats-0",
            ListenerBind::Socket {
                address: "127.0.0.1:9901".parse().unwrap(),
                ipv6_only: false,
            },
        ),
    ];
    entries[2].metric_policy = ListenerMetricPolicy::InternalOnly;
    entries[3].metric_policy = ListenerMetricPolicy::InternalOnly;
    metrics
        .register_inventory(&entries)
        .unwrap()
        .commit()
        .unwrap();

    let public = metrics.snapshot().unwrap();
    assert_eq!(
        public
            .listeners
            .iter()
            .map(|listener| listener.name.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "zulu"]
    );
    assert_eq!(
        metrics
            .topology_health_snapshot()
            .unwrap()
            .listeners
            .iter()
            .map(|listener| listener.name.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "zulu"]
    );
    assert_eq!(
        metrics
            .internal_listener_snapshots()
            .unwrap()
            .iter()
            .map(|listener| listener.name.as_str())
            .collect::<Vec<_>>(),
        ["zulu", "alpha", "@management", "@stats-0"]
    );
    let bytes = serde_json::to_vec(&public.listeners).unwrap();
    assert!(
        bytes
            .windows(b"alpha".len())
            .position(|value| value == b"alpha")
            < bytes
                .windows(b"zulu".len())
                .position(|value| value == b"zulu")
    );
    assert!(
        !bytes
            .windows(b"@management".len())
            .any(|value| value == b"@management")
    );
}

#[test]
fn the_worker_report_matches_the_full_snapshot_without_sampling_the_host() {
    let metrics = RuntimeMetrics::for_process(ProcessRuntime::new(None));
    let mut entries = vec![
        inventory_entry(
            ListenerId::Traffic("web".into()),
            "web",
            ListenerBind::Socket {
                address: "127.0.0.1:8080".parse().unwrap(),
                ipv6_only: false,
            },
        ),
        inventory_entry(
            ListenerId::Management,
            "@management",
            ListenerBind::Socket {
                address: "127.0.0.1:9900".parse().unwrap(),
                ipv6_only: false,
            },
        ),
    ];
    entries[1].metric_policy = ListenerMetricPolicy::InternalOnly;
    metrics
        .register_inventory(&entries)
        .unwrap()
        .commit()
        .unwrap();

    let report = metrics.worker_report().unwrap();
    let snapshot = metrics.snapshot().unwrap();

    assert_eq!(report.traffic, snapshot.traffic);
    assert_eq!(
        report.listeners,
        metrics.internal_listener_snapshots().unwrap()
    );
    assert_eq!(
        report.administrative_state,
        snapshot.process.administrative_state
    );
}

#[test]
fn administration_targets_public_traffic_when_hidden_control_name_collides() {
    let process = ProcessRuntime::new(None);
    let metrics = RuntimeMetrics::for_process(process);
    let mut entries = vec![
        inventory_entry(
            ListenerId::Traffic("@management".into()),
            "@management",
            ListenerBind::Socket {
                address: "127.0.0.1:8080".parse().unwrap(),
                ipv6_only: false,
            },
        ),
        inventory_entry(
            ListenerId::Management,
            "@management",
            ListenerBind::Socket {
                address: "127.0.0.1:9900".parse().unwrap(),
                ipv6_only: false,
            },
        ),
    ];
    entries[1].metric_policy = ListenerMetricPolicy::InternalOnly;
    metrics
        .register_inventory(&entries)
        .unwrap()
        .commit()
        .unwrap();

    metrics
        .set_listener_administrative_state("@management", AdministrativeState::Drain)
        .unwrap();

    assert_eq!(
        metrics
            .internal_listener_snapshots()
            .unwrap()
            .iter()
            .map(|listener| listener.administrative_state)
            .collect::<Vec<_>>(),
        [AdministrativeState::Drain, AdministrativeState::Ready]
    );
}

#[test]
fn internal_listener_view_preserves_real_counters_without_changing_public_projection() {
    let metrics = RuntimeMetrics::new();
    let public = metrics
        .register_listener_with_policy(
            ListenerId::Legacy("public".into()),
            "public".into(),
            "http".into(),
            "socket:127.0.0.1:8080".into(),
            None,
            ListenerMetricPolicy::Public,
        )
        .expect("public listener");
    let internal = metrics
        .register_listener_with_policy(
            ListenerId::Legacy("@management".into()),
            "@management".into(),
            "http".into(),
            "socket:127.0.0.1:9900".into(),
            None,
            ListenerMetricPolicy::InternalOnly,
        )
        .expect("internal listener");
    public.mark_listening();
    internal.mark_listening();
    let connection = internal
        .begin_control_connection()
        .expect("internal control connection");

    let public_snapshot = metrics.snapshot().expect("public snapshot");
    assert_eq!(
        public_snapshot
            .listeners
            .iter()
            .map(|listener| listener.name.as_str())
            .collect::<Vec<_>>(),
        vec!["public"]
    );
    assert_eq!(public_snapshot.traffic.accepted_connections, 0);

    let internal_snapshot = metrics
        .internal_listener_snapshots()
        .expect("internal snapshot");
    assert_eq!(internal_snapshot.len(), 2);
    let management = internal_snapshot
        .iter()
        .find(|listener| listener.name == "@management")
        .expect("management listener");
    assert_eq!(management.state, ListenerRuntimeState::Listening);
    assert_eq!(management.accepted_connections, 1);
    assert_eq!(management.active_connections, 1);

    drop(connection);
    let management = metrics
        .internal_listener_snapshots()
        .expect("released internal snapshot")
        .into_iter()
        .find(|listener| listener.name == "@management")
        .expect("management listener");
    assert_eq!(management.accepted_connections, 1);
    assert_eq!(management.active_connections, 0);
}

#[test]
fn process_uptime_admission_and_listener_capacity_survive_generation_overlap() {
    let process = ProcessRuntime::new(Some(2));
    let first = RuntimeMetrics::for_process(process.clone());
    let first_listener = first
        .register_listener("old-name", "tcp", "socket:127.0.0.1:8080", Some(1))
        .expect("first listener");
    first.activate_limits(Some(2));
    let held = first_listener.begin_connection().expect("held connection");
    let first_uptime = first.snapshot().expect("first snapshot").uptime_ms;

    let second = RuntimeMetrics::for_process(process);
    let second_listener = second
        .register_listener("new-name", "tcp", "socket:127.0.0.1:8080", Some(1))
        .expect("second listener");
    second.activate_limits(Some(2));

    assert!(matches!(
        second_listener.begin_connection(),
        Err(MetricsError::ConnectionLimitReached { limit: 1, .. })
    ));
    assert!(second.snapshot().expect("second snapshot").uptime_ms >= first_uptime);
    second.set_process_administrative_state(AdministrativeState::Drain);
    assert!(!first_listener.accepting());
    drop(held);
}

#[test]
fn listener_rejection_rolls_back_process_admission() {
    let runtime = RuntimeMetrics::with_max_connections(Some(2));
    let listener = runtime
        .register_listener("http", "http", "socket:127.0.0.1:8080", Some(1))
        .expect("listener");
    let admitted = listener.begin_connection().expect("first connection");
    assert!(matches!(
        listener.begin_connection(),
        Err(MetricsError::ConnectionLimitReached { limit: 1, .. })
    ));

    let process_only = runtime
        .begin_process_connection()
        .expect("listener rejection released process slot");
    assert!(matches!(
        runtime.begin_process_connection(),
        Err(MetricsError::ProcessConnectionLimitReached { limit: 2 })
    ));
    drop(process_only);
    drop(admitted);

    let listener = listener.state.snapshot();
    assert_eq!(listener.accepted_connections, 2);
    assert_eq!(listener.rejected_connections, 1);
    assert_eq!(listener.active_connections, 0);
    assert_eq!(
        runtime
            .inner
            .process_admission
            .active
            .load(Ordering::Relaxed),
        0
    );
    assert_eq!(
        runtime
            .inner
            .process_admission
            .rejected
            .load(Ordering::Relaxed),
        1
    );
}

#[test]
fn retry_attempts_saturate_without_wrapping() {
    let runtime = RuntimeMetrics::new();
    let listener = runtime
        .register_listener("http", "http", "socket:127.0.0.1:8080", None)
        .expect("listener");
    listener
        .process
        .retry_attempts
        .store(u64::MAX, Ordering::Relaxed);

    listener.record_retry_attempt();

    assert_eq!(
        runtime.snapshot().expect("snapshot").process.retry_attempts,
        u64::MAX
    );
}

#[test]
fn parses_process_cpu_ticks_when_command_contains_parentheses() {
    let stat = "42 (worker ) name) S 1 2 3 4 5 6 7 8 9 10 120 30\n";

    assert_eq!(parse_process_stat(stat).expect("process stat"), 150);
}

#[test]
fn parses_process_status_units() {
    let status = "Name:\toxiroute\nVmSize:\t2048 kB\nVmRSS:\t512 kB\nThreads:\t7\n";

    assert_eq!(
        parse_process_status(status).expect("process status"),
        ProcessStatus {
            resident_memory_bytes: 512 * 1024,
            virtual_memory_bytes: 2048 * 1024,
            thread_count: 7,
        }
    );
}

#[test]
fn rejects_process_status_with_missing_metrics() {
    let error = parse_process_status("VmRSS:\t512 kB\nThreads:\t7\n")
        .expect_err("missing virtual memory must fail");

    assert!(error.to_string().contains("VmSize"));
}

#[test]
fn parses_system_cpu_ticks_without_double_counting_guest_fields() {
    let stat = "cpu  1 2 3 4 5 6 7 8 100 200\ncpu0 1 2 3 4 5 6 7 8 50 100\ncpu1 0 0 0 0 0 0 0 0 0 0\nintr 12\n";

    assert_eq!(
        parse_system_stat(stat).expect("system stat"),
        CpuSample {
            process_ticks: 0,
            system_ticks: 36,
            logical_cpu_count: 2,
        }
    );
}

#[test]
fn parses_load_and_memory_fixtures() {
    let load_average = parse_load_average("0.25 1.50 2.75 1/100 42\n").expect("load average");

    assert!((load_average[0] - 0.25).abs() < f64::EPSILON);
    assert!((load_average[1] - 1.5).abs() < f64::EPSILON);
    assert!((load_average[2] - 2.75).abs() < f64::EPSILON);
    assert_eq!(
        parse_memory_info("MemTotal: 4096 kB\nMemFree: 100 kB\nMemAvailable: 1024 kB\n")
            .expect("memory info"),
        MemoryInfo {
            total_memory_bytes: 4096 * 1024,
            available_memory_bytes: 1024 * 1024,
        }
    );
}

#[test]
fn computes_cpu_percent_from_process_and_aggregate_ticks() {
    let previous = CpuSample {
        process_ticks: 100,
        system_ticks: 10_000,
        logical_cpu_count: 4,
    };
    let current = CpuSample {
        process_ticks: 125,
        system_ticks: 10_100,
        logical_cpu_count: 4,
    };

    assert_eq!(
        cpu_percent(Some(&previous), &current).expect("CPU percent"),
        Some(100.0)
    );
    assert_eq!(cpu_percent(None, &current).expect("first sample"), None);
}

#[test]
fn serving_matches_the_full_snapshot_without_sampling_the_host() {
    let process = ProcessRuntime::new(None);
    let metrics = RuntimeMetrics::for_process(process);
    let entry = inventory_entry(
        ListenerId::Traffic("edge".into()),
        "edge",
        ListenerBind::Socket {
            address: "127.0.0.1:8080".parse().expect("edge address"),
            ipv6_only: false,
        },
    );
    metrics
        .register_inventory(std::slice::from_ref(&entry))
        .unwrap()
        .commit()
        .unwrap();
    let listener = metrics
        .inventory_listener(&entry.id)
        .unwrap()
        .expect("listener");
    let snapshot_serving = |metrics: &RuntimeMetrics| metrics.snapshot().unwrap().serving();

    assert!(!metrics.serving().unwrap());
    assert_eq!(metrics.serving().unwrap(), snapshot_serving(&metrics));
    listener.mark_listening();
    assert!(metrics.serving().unwrap());
    assert_eq!(metrics.serving().unwrap(), snapshot_serving(&metrics));
    metrics.set_process_administrative_state(AdministrativeState::Drain);
    assert!(!metrics.serving().unwrap());
    assert_eq!(metrics.serving().unwrap(), snapshot_serving(&metrics));
    metrics.set_process_administrative_state(AdministrativeState::Ready);
    listener.mark_stopped();
    assert!(!metrics.serving().unwrap());
    assert_eq!(metrics.serving().unwrap(), snapshot_serving(&metrics));
}

#[test]
fn public_listener_snapshots_are_the_listeners_of_the_full_snapshot() {
    let metrics = RuntimeMetrics::for_process(ProcessRuntime::new(None));
    let entry = inventory_entry(
        ListenerId::Traffic("edge".into()),
        "edge",
        ListenerBind::Socket {
            address: "127.0.0.1:8080".parse().expect("edge address"),
            ipv6_only: false,
        },
    );
    metrics
        .register_inventory(std::slice::from_ref(&entry))
        .unwrap()
        .commit()
        .unwrap();
    metrics
        .inventory_listener(&entry.id)
        .unwrap()
        .expect("listener")
        .mark_listening();

    assert_eq!(
        metrics.public_listener_snapshots().unwrap(),
        metrics.snapshot().unwrap().listeners
    );
}
