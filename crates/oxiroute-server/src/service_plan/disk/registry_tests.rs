use super::*;

static SNAPSHOT_HOOK_TEST_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn incompatible_open_does_not_hold_registry_lock_while_final_backend_retires() {
    let _test_guard = SNAPSHOT_HOOK_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let directory = tempfile::tempdir().expect("disk registry root");
    let root = directory.path().join("cache");
    let config = DiskCacheConfig::default();
    let backend = open_shared_disk_backend(&root, &config).expect("initial backend");
    let mut incompatible = config.clone();
    incompatible.max_disk_bytes += 1;
    let (_hook, reached, release) = install_disk_registry_snapshot_hook(root.clone());

    let opener_root = root.clone();
    let opener_config = incompatible.clone();
    let opener = std::thread::spawn(move || open_shared_disk_backend(&opener_root, &opener_config));
    reached.wait();
    drop(backend);
    release.wait();

    let replacement = opener
        .join()
        .expect("incompatible opener thread")
        .expect("retired entry permits replacement config");
    assert_eq!(replacement.disk_config(), &incompatible);
}

#[test]
fn concurrent_compatible_opens_publish_one_shared_backend() {
    let directory = tempfile::tempdir().expect("disk registry root");
    let root = directory.path().join("cache");
    let start = Arc::new(std::sync::Barrier::new(3));
    let open = |start: Arc<std::sync::Barrier>, root: PathBuf| {
        std::thread::spawn(move || {
            start.wait();
            open_shared_disk_backend(&root, &DiskCacheConfig::default())
                .expect("compatible backend")
        })
    };
    let first = open(Arc::clone(&start), root.clone());
    let second = open(Arc::clone(&start), root.clone());
    start.wait();

    let first = first.join().expect("first opener");
    let second = second.join().expect("second opener");
    assert!(Arc::ptr_eq(&first, &second));
    drop(first);
    drop(second);
    assert!(!disk_backend_registry_contains(&root));
}

#[test]
fn compatible_waiter_times_out_without_removing_the_opening_insertion() {
    let directory = tempfile::tempdir().expect("disk registry root");
    let root = directory.path().join("cache");
    let config = DiskCacheConfig::default();
    let (_hook, reached, release) = install_disk_registry_opening_hook(root.clone());
    let opener_root = root.clone();
    let opener_config = config.clone();
    let opener = std::thread::spawn(move || {
        open_shared_disk_backend(&opener_root, &opener_config).expect("opening backend")
    });
    reached.wait();

    let deadline = Instant::now() + Duration::from_millis(25);
    let Err(error) = open_shared_disk_backend_with_deadline(&root, &config, Some(deadline)) else {
        panic!("compatible waiter did not time out")
    };

    assert_eq!(error, DiskBackendOpenError::TimedOut);
    assert!(Instant::now() >= deadline);
    assert!(disk_backend_registry_contains(&root));
    release.wait();
    let backend = opener.join().expect("opener thread");
    drop(backend);
    assert!(!disk_backend_registry_contains(&root));
}

#[test]
fn stale_snapshot_cleanup_does_not_remove_replacement_backend() {
    let _test_guard = SNAPSHOT_HOOK_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let directory = tempfile::tempdir().expect("disk registry root");
    let root = directory.path().join("cache");
    let config = DiskCacheConfig::default();
    let original = open_shared_disk_backend(&root, &config).expect("original backend");
    let (hook, reached, release) = install_disk_registry_snapshot_hook(root.clone());

    let stale_root = root.clone();
    let stale_config = config.clone();
    let stale = std::thread::spawn(move || {
        open_shared_disk_backend(&stale_root, &stale_config).expect("stale opener")
    });
    reached.wait();
    drop(hook);
    drop(original);
    let replacement = open_shared_disk_backend(&root, &config).expect("replacement backend");
    release.wait();

    let observed = stale.join().expect("stale opener thread");
    assert!(Arc::ptr_eq(&observed, &replacement));
    assert!(disk_backend_registry_contains(&root));
    drop(observed);
    drop(replacement);
    assert!(!disk_backend_registry_contains(&root));
}
