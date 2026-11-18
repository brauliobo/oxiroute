use std::{collections::BTreeSet, fs, path::PathBuf};

use openssl::sha::{Sha256, sha256};
use oxiroute_config::ValidatedConfig;
use serde_json::Value;

use super::EffectiveRevision;

pub(super) const EXTERNAL_FILE_KEYS: [&str; 9] = [
    "admin_token_file",
    "token_file_path",
    "htpasswd_file_path",
    "client_ca_file_path",
    "secret_file",
    "dh_parameters_path",
    "ca_certificate_path",
    "tokens_file",
    "credential_file",
];

pub(super) type Digest = Option<[u8; 32]>;

pub(super) fn digest(config: &ValidatedConfig) -> Digest {
    let paths = paths(config);
    if paths.is_empty() {
        return None;
    }
    let mut hasher = Sha256::new();
    for path in &paths {
        let name = path.as_os_str().as_encoded_bytes();
        hasher.update(&(name.len() as u64).to_be_bytes());
        hasher.update(name);
        match fs::read(path) {
            Ok(content) => {
                hasher.update(&[1]);
                hasher.update(&(content.len() as u64).to_be_bytes());
                hasher.update(&content);
            }
            Err(_) => hasher.update(&[0]),
        }
    }
    Some(hasher.finish())
}

pub(super) fn revision(canonical_kdl_digest: [u8; 32], external: Digest) -> EffectiveRevision {
    match external {
        None => EffectiveRevision::from_digest(canonical_kdl_digest),
        Some(external) => {
            let mut combined = canonical_kdl_digest.to_vec();
            combined.extend_from_slice(&external);
            EffectiveRevision::from_digest(sha256(&combined))
        }
    }
}

fn paths(config: &ValidatedConfig) -> BTreeSet<PathBuf> {
    let draft = config.as_draft();
    let mut paths = BTreeSet::new();
    if let Some(management) = &draft.management {
        paths.extend(crate::rtmp::api::management_token_file(management));
    }
    collect(
        &serde_json::to_value(draft).expect("a validated configuration serializes"),
        &mut paths,
    );
    paths
}

fn collect(value: &Value, paths: &mut BTreeSet<PathBuf>) {
    match value {
        Value::Object(fields) => {
            for (key, field) in fields {
                match field {
                    Value::String(path) if EXTERNAL_FILE_KEYS.contains(&key.as_str()) => {
                        paths.insert(PathBuf::from(path));
                    }
                    _ => collect(field, paths),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| collect(item, paths)),
        _ => {}
    }
}
