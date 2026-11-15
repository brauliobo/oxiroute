#[cfg(not(all(target_os = "linux", feature = "jemalloc")))]
use std::process::Command;

#[global_allocator]
static ALLOCATOR: oxiroute_allocator::Allocator = oxiroute_allocator::Allocator;

#[repr(align(4096))]
struct Aligned([u8; 8192]);

#[test]
fn selected_global_allocator_supports_alignment_and_growth() {
    let aligned = Box::new(Aligned([17; 8192]));
    assert_eq!(std::ptr::from_ref(&*aligned).addr() % 4096, 0);
    assert_eq!(aligned.0[8191], 17);

    let mut bytes = vec![0_u8; 32];
    bytes.resize(8192, 19);
    assert!(bytes[..32].iter().all(|byte| *byte == 0));
    assert!(bytes[32..].iter().all(|byte| *byte == 19));
    oxiroute_allocator::start_background_thread().expect("initialize the selected allocator");
    oxiroute_allocator::release_unused();
}

#[test]
fn backend_matches_the_build_features() {
    #[cfg(all(target_os = "linux", feature = "jemalloc"))]
    {
        let _: tikv_jemallocator::Jemalloc = oxiroute_allocator::SelectedAllocator;
        assert_eq!(oxiroute_allocator::NAME, "jemalloc");
    }
    #[cfg(not(all(target_os = "linux", feature = "jemalloc")))]
    {
        let _: std::alloc::System = oxiroute_allocator::SelectedAllocator;
        assert_eq!(oxiroute_allocator::NAME, "system");
    }
}

#[cfg(not(all(target_os = "linux", feature = "jemalloc")))]
#[test]
fn system_allocator_ignores_jemalloc_configuration() {
    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "selected_global_allocator_supports_alignment_and_growth",
        ])
        .env(
            "_RJEM_MALLOC_CONF",
            "abort_conf:true,unknown_allocator_option:true",
        )
        .output()
        .expect("run the system allocator test with jemalloc options");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(feature = "count")]
#[test]
fn counting_tracks_the_selected_global_allocator() {
    let counters = || {
        oxiroute_allocator::snapshot()
            .split_whitespace()
            .map(|value| value.parse::<u64>().expect("counter"))
            .collect::<Vec<_>>()
    };
    let before = counters();
    let bytes = std::hint::black_box(vec![23_u8; 1 << 16]);
    let after = counters();
    assert_eq!(bytes[65535], 23);
    assert!(after[0] > before[0]);
    assert!(after[5] >= before[5] + 65536);
}
