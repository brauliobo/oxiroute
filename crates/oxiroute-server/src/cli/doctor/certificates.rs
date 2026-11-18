use std::{
    fs,
    path::{Path, PathBuf},
};

use openssl::x509::X509;
use oxiroute_config::{CertificateSource, ConfigDraft};

use super::diagnosis::{Check, Diagnosis};
use crate::cli::tls::Expiry;

const WARN_SECONDS: i64 = 30 * 86_400;
const CRITICAL_SECONDS: i64 = 7 * 86_400;

fn chain_path(source: &CertificateSource) -> Option<PathBuf> {
    match source {
        CertificateSource::Files {
            certificate_chain_path,
            ..
        } => Some(certificate_chain_path.clone()),
        CertificateSource::Certbot {
            live_directory_path,
            ..
        } => Some(live_directory_path.join("fullchain.pem")),
        CertificateSource::AcmeManaged { .. } | CertificateSource::SelfSignedDevelopment { .. } => {
            None
        }
    }
}

fn expiry_of(path: &Path) -> Result<Expiry, String> {
    let pem = fs::read(path).map_err(|error| format!("cannot be read: {error}"))?;
    let leaf =
        X509::from_pem(&pem).map_err(|error| format!("is not a PEM certificate: {error}"))?;
    Expiry::of_time(leaf.not_after()).map_err(|error| error.to_string())
}

fn judge(name: &str, path: &Path, now: i64) -> Diagnosis {
    let subject = path.display().to_string();
    let expiry = match expiry_of(path) {
        Ok(expiry) => expiry,
        Err(reason) => {
            return Diagnosis::critical(
                Check::Certificates,
                subject,
                format!("certificate {name} {reason}"),
            );
        }
    };
    match expiry.finding(name, WARN_SECONDS, CRITICAL_SECONDS, now) {
        Some(finding) => Diagnosis::new(
            finding.severity,
            Check::Certificates,
            subject,
            finding.message,
        )
        .fixed_by("renew the certificate files, then `oxiroute generation reload`"),
        None => Diagnosis::ok(
            Check::Certificates,
            subject,
            format!("{name} expires {} ({})", expiry.utc(), expiry.relative(now)),
        ),
    }
}

pub(super) fn check(draft: &ConfigDraft, now: i64) -> Vec<Diagnosis> {
    draft
        .certificates
        .iter()
        .filter_map(|certificate| Some((certificate, chain_path(&certificate.source)?)))
        .map(|(certificate, path)| judge(&certificate.name, &path, now))
        .collect()
}
