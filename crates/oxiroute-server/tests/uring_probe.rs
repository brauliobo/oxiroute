#![cfg(target_os = "linux")]

fn rings_open() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .expect("descriptor table")
        .flatten()
        .filter_map(|entry| std::fs::read_link(entry.path()).ok())
        .filter(|target| target.to_string_lossy().contains("io_uring"))
        .count()
}

#[test]
fn probing_io_uring_leaves_no_ring_open() {
    let before = rings_open();
    for _ in 0..8 {
        let _ = oxiroute_epoll_h1::io_uring_support();
    }
    assert_eq!(rings_open(), before);
}
