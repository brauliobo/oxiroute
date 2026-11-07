#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn transaction_lock_is_mode_restricted_and_never_follows_symlinks() {
    let (temp, path, coordinator) = fixture(&minimal_config());
    let expected = loaded(coordinator.load()).authored_revision;
    let ConfigSaveOutcome::Saved(first) = save_draft(&coordinator, &expected, rtmp_config("first"))
    else {
        panic!("initial save failed")
    };
    let lock_path = lock_entry(temp.path()).expect("transaction lock entry");
    assert_eq!(
        fs::symlink_metadata(&lock_path).unwrap().mode() & 0o7777,
        0o600
    );

    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o644)).unwrap();
    let outcome = CanonicalConfigCoordinator::new(&path).unwrap().save(
        &first.authored_revision,
        prepared(&coordinator, rtmp_config("wrong-mode")),
    );
    let ConfigSaveOutcome::Failed(failure) = outcome else {
        panic!("permissive transaction lock was accepted")
    };
    assert_eq!(failure.diagnostics[0].code, "E_CONFIG_LOCK");

    fs::remove_file(&lock_path).unwrap();
    let outside = temp.path().join("outside.lock");
    fs::write(&outside, []).unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&outside, &lock_path).unwrap();
    let outcome = CanonicalConfigCoordinator::new(&path).unwrap().save(
        &first.authored_revision,
        prepared(&coordinator, rtmp_config("symlink")),
    );
    let ConfigSaveOutcome::Failed(failure) = outcome else {
        panic!("symbolic transaction lock was followed")
    };
    assert_eq!(failure.diagnostics[0].code, "E_CONFIG_LOCK");
    assert_eq!(
        loaded(CanonicalConfigCoordinator::new(&path).unwrap().load())
            .validated_config
            .as_draft(),
        &rtmp_config("first")
    );
}

#[test]
fn symlink_and_special_file_targets_are_rejected_without_following() {
    let (temp, path, coordinator) = fixture(&minimal_config());
    let expected = loaded(coordinator.load()).authored_revision;
    let outside = temp.path().join("outside.lua");
    let outside_bytes = render_config(
        ConfigFormat::Lua,
        &rtmp_config("outside").validate().unwrap(),
    )
    .unwrap();
    fs::write(&outside, &outside_bytes).unwrap();
    fs::remove_file(&path).unwrap();
    symlink(&outside, &path).unwrap();

    let outcome = save_draft(&coordinator, &expected, rtmp_config("candidate"));

    assert!(matches!(outcome, ConfigSaveOutcome::Failed(_)));
    assert_eq!(fs::read_to_string(&outside).unwrap(), outside_bytes);
    assert!(
        fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );

    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    let ConfigLoadOutcome::Rejected(rejection) = coordinator.load() else {
        panic!("special file was loaded")
    };
    assert_eq!(rejection.diagnostics[0].code, "E_CONFIG_FILE_TYPE");
}

#[test]
fn intermediate_parent_symlinks_are_rejected_without_following() {
    let temp = TempDir::new().unwrap();
    let real_parent = temp.path().join("real");
    let nested_parent = real_parent.join("nested");
    fs::create_dir_all(&nested_parent).unwrap();
    let real_path = nested_parent.join("oxiroute.lua");
    let original = render_config(ConfigFormat::Lua, &minimal_config().validate().unwrap()).unwrap();
    fs::write(&real_path, &original).unwrap();
    let linked_parent = temp.path().join("linked");
    symlink(&real_parent, &linked_parent).unwrap();
    let linked_path = linked_parent.join("nested/oxiroute.lua");
    let coordinator = CanonicalConfigCoordinator::new(&linked_path).unwrap();

    let ConfigLoadOutcome::Rejected(rejection) = coordinator.load() else {
        panic!("configuration beneath an intermediate symlink was loaded")
    };
    assert_eq!(rejection.diagnostics[0].code, "E_CONFIG_READ");

    let outcome = coordinator.save(
        &AuthoredRevision::from_bytes(original.as_bytes()),
        prepared(&coordinator, rtmp_config("candidate")),
    );
    assert!(matches!(outcome, ConfigSaveOutcome::Failed(_)));
    assert_eq!(fs::read_to_string(real_path).unwrap(), original);
}

#[test]
fn pinned_parent_chain_rejects_an_intermediate_symlink_rebind() {
    let temp = TempDir::new().unwrap();
    let ancestor = temp.path().join("ancestor");
    let nested_parent = ancestor.join("nested");
    fs::create_dir_all(&nested_parent).unwrap();
    let path = nested_parent.join("oxiroute.lua");
    let original = render_config(ConfigFormat::Lua, &minimal_config().validate().unwrap()).unwrap();
    fs::write(&path, &original).unwrap();
    let storage = CanonicalStorage::open(&path).unwrap();

    let moved_ancestor = temp.path().join("moved");
    fs::rename(&ancestor, &moved_ancestor).unwrap();
    symlink(&moved_ancestor, &ancestor).unwrap();

    assert_eq!(storage.read(), Err(StorageFailure::DirectoryChanged));
    assert_eq!(
        fs::read_to_string(moved_ancestor.join("nested/oxiroute.lua")).unwrap(),
        original
    );
}

#[test]
fn oversized_and_unstable_reads_are_rejected() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("oxiroute.lua");
    fs::write(&path, vec![b'x'; MAX_CANONICAL_CONFIG_BYTES + 1]).unwrap();
    let coordinator = CanonicalConfigCoordinator::new(&path).unwrap();
    let ConfigLoadOutcome::Rejected(rejection) = coordinator.load() else {
        panic!("oversized file was loaded")
    };
    assert_eq!(rejection.diagnostics[0].code, "E_CONFIG_TOO_LARGE");

    fs::write(&path, b"first").unwrap();
    let storage = CanonicalStorage::open(&path).unwrap();
    let changed_path = path.clone();
    let result = storage.read_with_hook(|| fs::write(changed_path, b"second").unwrap());
    assert_eq!(result, Err(StorageFailure::Unstable));
}

#[test]
fn commit_sync_failure_rolls_back_to_the_old_file() {
    let (temp, path, coordinator) = fixture(&minimal_config());
    let before = fs::read(&path).unwrap();
    let expected = loaded(coordinator.load()).authored_revision;

    let outcome = coordinator.save_inner(
        &expected,
        prepared(&coordinator, rtmp_config("candidate")),
        || Ok(()),
        || {},
        ReplaceControl {
            fail_commit_sync: true,
            ..ReplaceControl::default()
        },
    );

    let ConfigSaveOutcome::Failed(failure) = outcome else {
        panic!("sync failure did not fail the save")
    };
    assert_eq!(failure.diagnostics[0].code, "E_CONFIG_DIRECTORY_SYNC");
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(no_temporary_entries(temp.path()));
}

#[test]
fn committed_cleanup_sync_failure_returns_saved_with_a_warning() {
    let (temp, path, coordinator) = fixture(&minimal_config());
    let expected = loaded(coordinator.load()).authored_revision;
    let candidate = rtmp_config("candidate");
    let candidate_bytes =
        render_config(ConfigFormat::Lua, &candidate.clone().validate().unwrap()).unwrap();

    let outcome = coordinator.save_inner(
        &expected,
        prepared(&coordinator, candidate),
        || Ok(()),
        || {},
        ReplaceControl {
            fail_cleanup_sync: true,
            ..ReplaceControl::default()
        },
    );

    let ConfigSaveOutcome::Saved(saved) = outcome else {
        panic!("committed cleanup sync failure was reported as unwritten")
    };
    assert_eq!(
        saved.authored_revision,
        AuthoredRevision::from_bytes(candidate_bytes.as_bytes())
    );
    assert_eq!(saved.diagnostics.len(), 1);
    assert_eq!(saved.diagnostics[0].code, "W_CONFIG_CLEANUP_DURABILITY");
    assert_eq!(
        saved.diagnostics[0].severity,
        ConfigDiagnosticSeverity::Warning
    );
    assert_eq!(saved.diagnostics[0].stage, ConfigDiagnosticStage::Sync);
    assert_eq!(fs::read_to_string(path).unwrap(), candidate_bytes);
    assert!(no_temporary_entries(temp.path()));
}

#[test]
fn rollback_cleanup_sync_failure_is_reported() {
    let (temp, path, coordinator) = fixture(&minimal_config());
    let before = fs::read(&path).unwrap();
    let expected = loaded(coordinator.load()).authored_revision;

    let outcome = coordinator.save_inner(
        &expected,
        prepared(&coordinator, rtmp_config("candidate")),
        || Ok(()),
        || {},
        ReplaceControl {
            fail_commit_sync: true,
            fail_cleanup_sync: true,
            ..ReplaceControl::default()
        },
    );

    let ConfigSaveOutcome::Failed(failure) = outcome else {
        panic!("rollback cleanup sync failure did not fail the save")
    };
    assert_eq!(failure.diagnostics[0].code, "E_CONFIG_ROLLBACK");
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(no_temporary_entries(temp.path()));
}

#[test]
fn failure_after_temp_sync_leaves_old_file_and_no_temp_entry() {
    let (temp, path, coordinator) = fixture(&minimal_config());
    let before = fs::read(&path).unwrap();
    let expected = loaded(coordinator.load()).authored_revision;

    let outcome = coordinator.save_inner(
        &expected,
        prepared(&coordinator, rtmp_config("candidate")),
        || Ok(()),
        || {},
        ReplaceControl {
            fail_before_exchange: true,
            ..ReplaceControl::default()
        },
    );

    assert!(matches!(outcome, ConfigSaveOutcome::Failed(_)));
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(no_temporary_entries(temp.path()));
}
