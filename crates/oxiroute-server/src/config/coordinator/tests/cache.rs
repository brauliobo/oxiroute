#[allow(clippy::wildcard_imports)]
use super::*;

fn resolutions(coordinator: &CanonicalConfigCoordinator) -> usize {
    coordinator.lock().resolutions
}

#[test]
fn unchanged_bytes_are_resolved_once() {
    let (_temp, _path, coordinator) = fixture(&normalizable_config());

    let first = loaded(coordinator.load());
    let second = loaded(coordinator.load());
    let shared = coordinator.load_shared().unwrap();

    assert_eq!(first, second);
    assert_eq!(*shared, first);
    assert_eq!(resolutions(&coordinator), 1);
}

#[test]
fn clones_of_a_coordinator_share_the_resolution() {
    let (_temp, _path, coordinator) = fixture(&normalizable_config());
    let clone = coordinator.clone();

    let first = loaded(coordinator.load());
    let second = loaded(clone.load());

    assert_eq!(first, second);
    assert_eq!(resolutions(&clone), 1);
}

#[test]
fn a_same_size_edit_that_restores_the_modification_time_is_resolved_again() {
    let (_temp, path, coordinator) = fixture(&normalizable_config());
    let before = loaded(coordinator.load());
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    let source = fs::read_to_string(&path).unwrap();
    let edited = source.replace("127.0.0.1:3000", "127.0.0.1:3001");
    assert_ne!(edited, source);
    assert_eq!(edited.len(), source.len());

    fs::write(&path, edited).unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    let after = loaded(coordinator.load());

    assert_ne!(after.authored_revision, before.authored_revision);
    assert_ne!(after.effective_revision, before.effective_revision);
    assert_eq!(resolutions(&coordinator), 2);
}

#[test]
fn a_rejected_edit_never_serves_the_previous_document() {
    let (_temp, path, coordinator) = fixture(&normalizable_config());
    let good = fs::read(&path).unwrap();
    let before = loaded(coordinator.load());

    fs::write(&path, b"return {").unwrap();
    assert!(matches!(coordinator.load(), ConfigLoadOutcome::Rejected(_)));
    fs::write(&path, good).unwrap();

    assert_eq!(loaded(coordinator.load()), before);
}

#[test]
fn kdl_documents_reuse_the_canonical_text_as_their_preview() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("oxiroute.kdl");
    let config = normalizable_config().validate().unwrap();
    let canonical = render_config(ConfigFormat::Kdl, &config).unwrap();
    fs::write(&path, &canonical).unwrap();
    let coordinator = CanonicalConfigCoordinator::new(&path).unwrap();

    let document = loaded(coordinator.load());

    assert_eq!(document.format, ConfigFormat::Kdl);
    assert_eq!(document.config_preview, canonical);
    assert_eq!(document.validated_config, config);
    let ConfigValidationOutcome::Valid(candidate) = coordinator.prepare(config.to_draft()) else {
        panic!("draft is invalid")
    };
    assert_eq!(candidate.config_preview(), canonical);
}
