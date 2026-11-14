//! Public certificate evidence shared by the importers: stable PEM reads and leaf identities.

use std::{
    fs::{File, Metadata},
    io::{BufReader, Read},
    net::IpAddr,
    path::Path,
    time::SystemTime,
};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use oxiroute_config::{canonical_certificate_dns_name, canonical_ip};
use rustls_pki_types::pem::{self, SectionKind};
use x509_parser::{extensions::GeneralName, parse_x509_certificate};

const MAX_CERTIFICATE_CHAIN_BYTES: usize = 1024 * 1024;
const MAX_CERTIFICATES_IN_CHAIN: usize = 16;
const MAX_CERTIFICATE_IDENTITIES: usize = 100;

pub(crate) type PemItems = Vec<(SectionKind, Vec<u8>)>;

pub(crate) struct PemMetadata {
    /// Leaf DNS and IP subject alternative names, canonicalized, in certificate order.
    pub(crate) identities: Vec<String>,
    pub(crate) leaf: Vec<u8>,
    pub(crate) private_keys: usize,
}

/// Reads the leaf subject alternative names of a public certificate file; never accepts key material.
pub(crate) fn leaf_identities(path: &Path, kind: &str) -> Result<Vec<String>, String> {
    let metadata = certificate_metadata(path, kind, &read_pem_items(path, kind)?)?;
    if metadata.private_keys != 0 {
        return Err(format!(
            "{kind} `{}` contains a private key",
            path.display()
        ));
    }
    Ok(metadata.identities)
}

pub(crate) fn read_pem_items(path: &Path, kind: &str) -> Result<PemItems, String> {
    let bytes = read_stable_pem(path, kind)?;
    let mut reader = BufReader::new(bytes.as_slice());
    let mut items = Vec::new();
    while let Some((section, data)) = pem::from_buf(&mut reader)
        .map_err(|error| format!("cannot parse {kind} `{}`: {error}", path.display()))?
    {
        if matches!(
            section,
            SectionKind::Certificate
                | SectionKind::PublicKey
                | SectionKind::RsaPrivateKey
                | SectionKind::PrivateKey
                | SectionKind::EcPrivateKey
                | SectionKind::Crl
                | SectionKind::Csr
        ) {
            items.push((section, data));
        }
    }
    Ok(items)
}

pub(crate) fn certificate_metadata(
    path: &Path,
    kind: &str,
    items: &PemItems,
) -> Result<PemMetadata, String> {
    let subject = format!("{kind} `{}`", path.display());
    let mut identities = Vec::new();
    let mut leaf = None;
    let mut certificates = 0usize;
    let mut private_keys = 0usize;
    for (section, data) in items {
        match section {
            SectionKind::Certificate => {
                if certificates == 0 {
                    leaf = Some(data.clone());
                }
                collect_identities(&subject, data, certificates, &mut identities)?;
                certificates += 1;
            }
            SectionKind::RsaPrivateKey | SectionKind::PrivateKey | SectionKind::EcPrivateKey => {
                private_keys += 1;
            }
            _ => return Err(format!("{subject} contains an unsupported PEM item")),
        }
    }
    if certificates == 0 {
        return Err(format!("{subject} contains no certificates"));
    }
    if certificates > MAX_CERTIFICATES_IN_CHAIN {
        return Err(format!(
            "{subject} exceeds {MAX_CERTIFICATES_IN_CHAIN} certificates"
        ));
    }
    validate_identities(&subject, &identities)?;
    Ok(PemMetadata {
        identities,
        leaf: leaf.expect("nonempty chain has a leaf certificate"),
        private_keys,
    })
}

fn collect_identities(
    subject: &str,
    certificate: &[u8],
    index: usize,
    identities: &mut Vec<String>,
) -> Result<(), String> {
    let (remainder, parsed) = parse_x509_certificate(certificate)
        .map_err(|_| format!("{subject} contains an invalid X.509 certificate"))?;
    if !remainder.is_empty() {
        return Err(format!("{subject} contains trailing certificate DER data"));
    }
    let is_ca = parsed
        .basic_constraints()
        .map_err(|_| format!("{subject} has invalid basic constraints"))?
        .is_some_and(|constraints| constraints.value.ca);
    if index != 0 && !is_ca {
        return Err(format!(
            "{subject} contains multiple end-entity certificates; multi-cert selection is unsupported"
        ));
    }
    if index == 0
        && let Some(names) = parsed
            .subject_alternative_name()
            .map_err(|_| format!("{subject} has an invalid subject alternative name extension"))?
    {
        for name in &names.value.general_names {
            match name {
                GeneralName::DNSName(name) => {
                    let dns_name = canonical_certificate_dns_name(name).map_err(|_| {
                        format!("{subject} contains an unsupported DNS subject alternative name")
                    })?;
                    identities.push(dns_name);
                }
                GeneralName::IPAddress(octets) => identities.push(ip_identity(subject, octets)?),
                _ => {}
            }
        }
    }
    Ok(())
}

fn ip_identity(subject: &str, octets: &[u8]) -> Result<String, String> {
    let ip = <[u8; 4]>::try_from(octets)
        .map(IpAddr::from)
        .or_else(|_| <[u8; 16]>::try_from(octets).map(IpAddr::from))
        .map_err(|_| format!("{subject} contains an unsupported IP subject alternative name"))?;
    Ok(canonical_ip(ip).to_string())
}

fn validate_identities(subject: &str, identities: &[String]) -> Result<(), String> {
    if identities.is_empty() {
        return Err(format!("{subject} has no DNS subject alternative names"));
    }
    if identities.len() > MAX_CERTIFICATE_IDENTITIES {
        return Err(format!(
            "{subject} exceeds {MAX_CERTIFICATE_IDENTITIES} DNS subject alternative names"
        ));
    }
    let mut unique = std::collections::HashSet::with_capacity(identities.len());
    if identities.iter().all(|name| unique.insert(name)) {
        Ok(())
    } else {
        Err(format!("{subject} repeats a DNS subject alternative name"))
    }
}

pub(crate) fn read_stable_pem(path: &Path, kind: &str) -> Result<Vec<u8>, String> {
    let limit = u64::try_from(MAX_CERTIFICATE_CHAIN_BYTES).unwrap_or(u64::MAX);
    let mut file = File::open(path)
        .map_err(|error| format!("cannot open {kind} `{}`: {error}", path.display()))?;
    let before = file
        .metadata()
        .map_err(|error| format!("cannot inspect {kind} `{}`: {error}", path.display()))?;
    if !before.is_file() {
        return Err(format!("{kind} `{}` is not a regular file", path.display()));
    }
    if before.len() > limit {
        return Err(pem_size_error(path, kind));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {kind} `{}`: {error}", path.display()))?;
    let after = file
        .metadata()
        .map_err(|error| format!("cannot re-inspect {kind} `{}`: {error}", path.display()))?;
    if PemFingerprint::new(&before) != PemFingerprint::new(&after) {
        return Err(format!(
            "{kind} `{}` changed while metadata was read",
            path.display()
        ));
    }
    if bytes.len() > MAX_CERTIFICATE_CHAIN_BYTES {
        return Err(pem_size_error(path, kind));
    }
    Ok(bytes)
}

fn pem_size_error(path: &Path, kind: &str) -> String {
    format!(
        "{kind} `{}` exceeds {MAX_CERTIFICATE_CHAIN_BYTES} bytes",
        path.display()
    )
}

#[derive(Eq, PartialEq)]
struct PemFingerprint {
    length: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
}

impl PemFingerprint {
    fn new(metadata: &Metadata) -> Self {
        Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            changed_seconds: metadata.ctime(),
            #[cfg(unix)]
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}
