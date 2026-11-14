use std::path::{Path, PathBuf};

use oxiroute_config::CertificateSource;

use crate::{
    certificate::leaf_identities,
    nginx::{EffectiveBind, EffectiveServer, ServerNameKind},
};

use super::super::{
    listener::{canonical_dns_name, canonical_exact_host, canonical_wildcard_host},
    provenance::utf8,
};

pub(super) fn certificate_source(chain: PathBuf, key: PathBuf) -> CertificateSource {
    let live_directory = chain.parent();
    let certbot_root = live_directory.and_then(Path::parent);
    let certbot_domain = live_directory.and_then(Path::file_name);
    if chain
        .file_name()
        .is_some_and(|name| name == "fullchain.pem")
        && key.file_name().is_some_and(|name| name == "privkey.pem")
        && key.parent() == live_directory
        && certbot_root
            .and_then(Path::file_name)
            .is_some_and(|name| name == "live")
    {
        let root = certbot_root
            .and_then(Path::parent)
            .expect("live directory has a parent");
        return CertificateSource::Certbot {
            live_directory_path: live_directory
                .expect("certbot live directory")
                .to_path_buf(),
            archive_directory_path: root
                .join("archive")
                .join(certbot_domain.expect("certbot domain directory")),
        };
    }
    CertificateSource::Files {
        certificate_chain_path: chain,
        private_key_path: key,
    }
}

/// Leaf SAN identities of a Certbot lineage, sorted. The runtime requires the declared names of every
/// certificate to equal them, so server names cannot stand in; other sources derive names from `server_name`.
pub(super) fn lineage_identities(
    source: &CertificateSource,
) -> Option<Result<Vec<String>, String>> {
    let CertificateSource::Certbot {
        live_directory_path,
        ..
    } = source
    else {
        return None;
    };
    Some(
        leaf_identities(&live_directory_path.join("cert.pem"), "nginx certificate").map(
            |mut identities| {
                identities.sort();
                identities
            },
        ),
    )
}

/// Exact, wildcard and leading-dot `server_name`s that the server answers on this bind, as certificate identities.
pub(super) fn declared_identities(bind: &EffectiveBind, server: &EffectiveServer) -> Vec<String> {
    let mut identities = Vec::new();
    for name in bind
        .names
        .iter()
        .filter(|name| name.server == server.origin.occurrence)
        .filter_map(|name| utf8(&name.name.normalized).map(|text| (name.name.kind, text)))
    {
        match name {
            (ServerNameKind::Exact, name) => identities.extend(canonical_exact_host(name)),
            (ServerNameKind::LeadingWildcard, name) if canonical_wildcard_host(name) => {
                identities.push(name.to_ascii_lowercase());
            }
            (ServerNameKind::LeadingWildcardAndExact, name) => {
                if let Some(suffix) = name
                    .strip_prefix('.')
                    .filter(|suffix| canonical_dns_name(suffix))
                {
                    identities.push(suffix.to_ascii_lowercase());
                    identities.push(format!("*.{}", suffix.to_ascii_lowercase()));
                }
            }
            _ => {}
        }
    }
    identities.sort();
    identities.dedup();
    identities
}

/// Declared identities that no SAN matches: exactly, or a one-label wildcard for an exact name.
pub(super) fn uncovered_identities(declared: &[String], sans: &[String]) -> Vec<String> {
    let covered = |name: &String| {
        sans.contains(name)
            || name.split_once('.').is_some_and(|(_, parent)| {
                sans.contains(&format!("*.{parent}")) && !name.starts_with("*.")
            })
    };
    declared
        .iter()
        .filter(|name| !covered(name))
        .cloned()
        .collect()
}
