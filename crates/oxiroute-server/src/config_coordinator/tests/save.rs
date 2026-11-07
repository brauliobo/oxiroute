#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn typed_save_rejects_a_compositional_root_without_flattening_it() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("oxiroute.hocon");
    let source = b"templates = { empty = { listeners = [] } }\nuse = \"empty\"\nversion = 1\n";
    fs::write(&path, source).unwrap();
    let coordinator = CanonicalConfigCoordinator::new(&path).unwrap();
    let document = loaded(coordinator.load());

    assert!(document.compositional);
    let ConfigSaveOutcome::InvalidDraft(rejection) =
        save_draft(&coordinator, &document.authored_revision, minimal_config())
    else {
        panic!("compositional root was flattened")
    };
    assert_eq!(rejection.diagnostics[0].code, "E_COMPOSITIONAL_ROOT");
    assert_eq!(fs::read(&path).unwrap(), source);
}

#[test]
fn save_escapes_values_and_installs_a_secure_regular_file() {
    let (_temp, path, coordinator) = fixture(&minimal_config());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let expected = loaded(coordinator.load()).authored_revision;
    let draft = rtmp_config("edge \"quoted\" \\ slash café");

    let ConfigSaveOutcome::Saved(saved) = save_draft(&coordinator, &expected, draft.clone()) else {
        panic!("save failed")
    };

    let bytes = fs::read(&path).unwrap();
    assert_eq!(bytes, saved.config_preview.as_bytes());
    assert!(
        saved
            .config_preview
            .contains(r#"name = "edge \"quoted\" \\ slash café","#)
    );
    assert_eq!(fs::symlink_metadata(&path).unwrap().mode() & 0o7777, 0o600);
    assert!(fs::symlink_metadata(&path).unwrap().file_type().is_file());
    assert_eq!(
        loaded(coordinator.load()).validated_config.as_draft(),
        &draft
    );
}

#[test]
fn invalid_draft_is_redacted_and_never_touches_disk() {
    let (_temp, path, coordinator) = fixture(&minimal_config());
    let before = fs::read(&path).unwrap();
    let mut draft = rtmp_config("private-diagnostic-value");
    draft.version = 99;

    let ConfigValidationOutcome::Invalid(rejection) = coordinator.prepare(draft) else {
        panic!("draft was not rejected")
    };

    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(rejection.diagnostics[0].code, "E_INVALID_VALUE");
    assert!(!format!("{:?}", rejection.diagnostics).contains("private-diagnostic-value"));
}

#[test]
fn invalid_external_content_is_reported_without_rewriting_it() {
    let (_temp, path, coordinator) = fixture(&minimal_config());
    let invalid = b"return { version = secret_external_value }\n";
    fs::write(&path, invalid).unwrap();

    let ConfigLoadOutcome::Rejected(rejection) = coordinator.load() else {
        panic!("invalid external source was loaded")
    };

    assert_eq!(fs::read(&path).unwrap(), invalid);
    assert_eq!(
        rejection.disk_revision,
        Some(AuthoredRevision::from_bytes(invalid))
    );
    assert!(!format!("{:?}", rejection.diagnostics).contains("secret_external_value"));
}

#[test]
fn stale_expected_revision_conflicts_without_overwriting_external_change() {
    let (_temp, path, coordinator) = fixture(&minimal_config());
    let expected = loaded(coordinator.load()).authored_revision;
    let external = render_config(
        ConfigFormat::Lua,
        &rtmp_config("external").validate().unwrap(),
    )
    .unwrap();
    fs::write(&path, &external).unwrap();

    let ConfigSaveOutcome::Conflict(conflict) =
        save_draft(&coordinator, &expected, rtmp_config("candidate"))
    else {
        panic!("stale save did not conflict")
    };

    assert_eq!(conflict.expected_revision, expected);
    assert_eq!(
        conflict.disk_revision,
        AuthoredRevision::from_bytes(external.as_bytes())
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), external);
}

#[test]
fn exchange_point_race_is_detected_and_external_file_is_restored() {
    let (temp, path, coordinator) = fixture(&minimal_config());
    let expected = loaded(coordinator.load()).authored_revision;
    let external = render_config(
        ConfigFormat::Lua,
        &rtmp_config("racing-external").validate().unwrap(),
    )
    .unwrap();
    let raced_path = path.clone();

    let outcome = coordinator.save_inner(
        &expected,
        prepared(&coordinator, rtmp_config("candidate")),
        || fs::write(&raced_path, &external).map_err(|_| ()),
        || {},
        ReplaceControl::default(),
    );

    let ConfigSaveOutcome::Conflict(conflict) = outcome else {
        panic!("racing save did not conflict")
    };
    assert_eq!(
        conflict.disk_revision,
        AuthoredRevision::from_bytes(external.as_bytes())
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), external);
    assert!(no_temporary_entries(temp.path()));
}

#[test]
fn post_exchange_replacement_conflicts_without_destroying_external_write() {
    let (temp, path, coordinator) = fixture(&minimal_config());
    let expected = loaded(coordinator.load()).authored_revision;
    let external = render_config(
        ConfigFormat::Lua,
        &rtmp_config("post-exchange-external").validate().unwrap(),
    )
    .unwrap();
    let external_path = temp.path().join("external.lua");
    fs::write(&external_path, &external).unwrap();
    let raced_path = path.clone();

    let outcome = coordinator.save_inner(
        &expected,
        prepared(&coordinator, rtmp_config("candidate")),
        || Ok(()),
        || fs::rename(&external_path, &raced_path).unwrap(),
        ReplaceControl::default(),
    );

    let ConfigSaveOutcome::Conflict(conflict) = outcome else {
        panic!("post-exchange replacement did not conflict")
    };
    assert_eq!(
        conflict.disk_revision,
        AuthoredRevision::from_bytes(external.as_bytes())
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), external);
    assert!(no_temporary_entries(temp.path()));
}

#[test]
fn post_exchange_in_place_write_conflicts_without_rolling_back_external_bytes() {
    let (temp, path, coordinator) = fixture(&minimal_config());
    let expected = loaded(coordinator.load()).authored_revision;
    let external = render_config(
        ConfigFormat::Lua,
        &rtmp_config("post-exchange-in-place").validate().unwrap(),
    )
    .unwrap();
    let raced_path = path.clone();

    let outcome = coordinator.save_inner(
        &expected,
        prepared(&coordinator, rtmp_config("candidate")),
        || Ok(()),
        || fs::write(&raced_path, &external).unwrap(),
        ReplaceControl::default(),
    );

    let ConfigSaveOutcome::Conflict(conflict) = outcome else {
        panic!("post-exchange in-place write did not conflict")
    };
    assert_eq!(
        conflict.disk_revision,
        AuthoredRevision::from_bytes(external.as_bytes())
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), external);
    assert!(no_temporary_entries(temp.path()));
}

#[test]
fn post_exchange_writer_is_not_destroyed_when_displaced_revision_also_conflicts() {
    let (temp, path, coordinator) = fixture(&minimal_config());
    let expected = loaded(coordinator.load()).authored_revision;
    let pre_exchange = render_config(
        ConfigFormat::Lua,
        &rtmp_config("pre-exchange-external").validate().unwrap(),
    )
    .unwrap();
    let post_exchange = render_config(
        ConfigFormat::Lua,
        &rtmp_config("post-exchange-external").validate().unwrap(),
    )
    .unwrap();
    let external_path = temp.path().join("external.lua");
    fs::write(&external_path, &post_exchange).unwrap();
    let before_path = path.clone();
    let after_path = path.clone();

    let outcome = coordinator.save_inner(
        &expected,
        prepared(&coordinator, rtmp_config("candidate")),
        || fs::write(&before_path, &pre_exchange).map_err(|_| ()),
        || fs::rename(&external_path, &after_path).unwrap(),
        ReplaceControl::default(),
    );

    let ConfigSaveOutcome::Conflict(conflict) = outcome else {
        panic!("two-sided exchange race did not conflict")
    };
    assert_eq!(
        conflict.disk_revision,
        AuthoredRevision::from_bytes(post_exchange.as_bytes())
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), post_exchange);
    assert!(no_temporary_entries(temp.path()));
}
