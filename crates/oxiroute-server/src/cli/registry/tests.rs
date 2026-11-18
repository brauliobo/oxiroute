use std::os::unix::fs::PermissionsExt as _;

use super::run::create_token;
use crate::registry::auth::{Scope, TokenStore};

#[test]
fn created_tokens_authenticate_once_and_ids_are_not_reused() {
    let directory = tempfile::tempdir().expect("directory");
    let file = directory.path().join("registry.tokens");
    let pools = ["voice".to_owned(), "render".to_owned()];

    let secret = create_token(&file, "agent-a", &pools, Some("gpu-"), false).expect("token");
    let read = create_token(&file, "dash", &[], None, true).expect("read token");

    assert_eq!(
        std::fs::metadata(&file).expect("file").permissions().mode() & 0o777,
        0o640
    );
    let text = std::fs::read_to_string(&file).expect("tokens");
    assert!(!text.contains(&secret), "only the digest is stored");
    let store = TokenStore::open(&file).expect("store");
    assert_eq!(
        store
            .authenticate(secret.as_bytes())
            .map(|principal| principal.scope),
        Some(Scope::Register {
            pools: pools.to_vec(),
            name_prefix: Some("gpu-".into()),
        })
    );
    assert_eq!(
        store
            .authenticate(read.as_bytes())
            .map(|principal| principal.scope),
        Some(Scope::Read)
    );
    assert!(create_token(&file, "agent-a", &pools, None, false).is_err());
    assert!(create_token(&file, "bad id", &pools, None, false).is_err());
}
