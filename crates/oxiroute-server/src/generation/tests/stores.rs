#[allow(clippy::wildcard_imports)]
use super::*;
use crate::StoreOwnershipError;

#[test]
fn candidates_prepare_without_owning_the_disk_cache_root() {
    let directory = TempDir::new().expect("disk cache root");
    let root = directory.path().join("cache");
    let manager = GenerationManager::new();

    let first = manager
        .prepare(disk_cache_document(&root))
        .expect("first candidate prepares");
    let second = GenerationManager::new()
        .prepare(disk_cache_document(&root))
        .expect("second candidate prepares while the first exists");

    assert!(!root.exists());
    drop((first, second));
}

#[test]
fn disk_cache_root_has_one_owner_across_a_release() {
    let directory = TempDir::new().expect("disk cache root");
    let root = directory.path().join("cache");
    let active = GenerationManager::new()
        .prepare(disk_cache_document(&root))
        .expect("active candidate");
    let replacement = GenerationManager::new()
        .prepare(disk_cache_document(&root))
        .expect("replacement candidate");
    let active_stores = active.generation().exclusive_stores();
    let replacement_stores = replacement.generation().exclusive_stores();

    active_stores.claim().expect("active generation claims");
    let Err(error) = replacement_stores.claim() else {
        panic!("replacement claimed a root the active generation owns")
    };
    let message = error.to_string();
    let StoreOwnershipError::DiskOwned { store, root: owned } = error else {
        panic!("wrong diagnostic: {message}")
    };
    assert_eq!(store, "disk");
    assert_eq!(owned, root);
    assert!(message.contains("`disk`"), "{message}");
    assert!(
        message.contains(root.to_str().expect("UTF-8 root")),
        "{message}"
    );
    assert!(message.contains("owned by the active worker"), "{message}");

    active_stores.release().expect("active generation releases");
    replacement_stores
        .claim()
        .expect("replacement claims after the release");
    replacement_stores.release().expect("replacement releases");
    active_stores
        .reclaim()
        .expect("the previous owner takes the root back");
}
