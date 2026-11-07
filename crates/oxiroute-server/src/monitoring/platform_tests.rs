use super::{system::unsupported_system_sample, *};

#[test]
fn unsupported_platform_fixture_has_no_fabricated_samples() {
    let sample = unsupported_system_sample();

    assert_eq!(sample.process_status.state, ComponentState::Unsupported);
    assert_eq!(sample.host.status.state, ComponentState::Unsupported);
    assert_eq!(sample.resident_memory_bytes, None);
    assert_eq!(sample.host.total_memory_bytes, None);
    assert_eq!(sample.host.load_average_1m, None);
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn linux_architecture_fixture_samples_process_and_host_metrics() {
    let sample = sample_system().expect("Linux process and host fixture");

    assert_eq!(sample.process_status.state, ComponentState::Healthy);
    assert_eq!(sample.host.status.state, ComponentState::Healthy);
    assert!(sample.resident_memory_bytes.is_some());
    assert!(sample.host.total_memory_bytes.is_some());
    assert!(sample.host.load_average_1m.is_some());
}
