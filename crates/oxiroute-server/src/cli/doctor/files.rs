use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
};

use oxiroute_config::{CertificateSource, ConfigDraft, HttpAccessPolicy};

use super::{
    diagnosis::{Check, Diagnosis},
    unit::Environment,
};
use crate::rtmp::api::{TOKEN_FILE_ENV, preflight_management_token, token_file_or};

const GROUP_AND_OTHERS: u32 = 0o077;
const OTHERS: u32 = 0o007;

fn mode_of(path: &Path) -> Result<u32, String> {
    fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o7777)
        .map_err(|error| format!("cannot be read: {error}"))
}

pub(super) fn config_file(path: &Path) -> Diagnosis {
    let subject = path.display().to_string();
    match mode_of(path) {
        Err(reason) => {
            Diagnosis::critical(Check::Files, subject, format!("the configuration {reason}"))
        }
        Ok(mode) if mode & 0o002 != 0 => Diagnosis::critical(
            Check::Files,
            &subject,
            format!("the configuration is writable by every user (mode {mode:04o})"),
        )
        .fixed_by(format!("chmod o-w {subject}")),
        Ok(mode) if mode & 0o020 != 0 => Diagnosis::warning(
            Check::Files,
            &subject,
            format!("the configuration is writable by its group (mode {mode:04o})"),
        )
        .fixed_by(format!("chmod g-w {subject}")),
        Ok(mode) => Diagnosis::ok(
            Check::Files,
            subject,
            format!("the configuration has mode {mode:04o}"),
        ),
    }
}

struct Secret {
    what: &'static str,
    forbidden: u32,
}

fn secrets(draft: &ConfigDraft) -> BTreeMap<PathBuf, Secret> {
    let mut secrets = BTreeMap::new();
    let mut add = |path: &Path, what, forbidden| {
        secrets.insert(path.to_owned(), Secret { what, forbidden });
    };
    for certificate in &draft.certificates {
        match &certificate.source {
            CertificateSource::Files {
                private_key_path, ..
            } => {
                add(private_key_path, "private key", GROUP_AND_OTHERS);
            }
            CertificateSource::AcmeManaged {
                dns01: Some(dns01), ..
            } => {
                add(
                    &dns01.credential_file,
                    "DNS credential file",
                    GROUP_AND_OTHERS,
                );
            }
            _ => {}
        }
    }
    for route in draft
        .http_services
        .iter()
        .flat_map(|service| &service.routes)
    {
        match &route.access_policy {
            Some(HttpAccessPolicy::BearerTokenFile {
                token_file_path, ..
            }) => {
                add(token_file_path, "access token file", OTHERS);
            }
            Some(HttpAccessPolicy::BasicHtpasswdFile {
                htpasswd_file_path, ..
            }) => {
                add(htpasswd_file_path, "htpasswd file", OTHERS);
            }
            None => {}
        }
    }
    if let Some(tokens) = draft
        .registry
        .as_ref()
        .and_then(|registry| registry.tokens_file.as_ref())
    {
        add(tokens, "registry token file", OTHERS);
    }
    secrets
}

fn judge(path: &Path, secret: &Secret) -> Option<Diagnosis> {
    let subject = path.display().to_string();
    let who = if secret.forbidden == OTHERS {
        "other users"
    } else {
        "its group and other users"
    };
    match mode_of(path) {
        Err(reason) => Some(Diagnosis::critical(
            Check::Files,
            subject,
            format!("the {} {reason}", secret.what),
        )),
        Ok(mode) if mode & secret.forbidden != 0 => Some(
            Diagnosis::warning(
                Check::Files,
                &subject,
                format!("the {} is readable by {who} (mode {mode:04o})", secret.what),
            )
            .fixed_by(format!(
                "chmod {} {subject}",
                if secret.forbidden == OTHERS {
                    "o-rwx"
                } else {
                    "go-rwx"
                }
            )),
        ),
        Ok(_) => None,
    }
}

fn management_token(draft: &ConfigDraft, environment: &Environment) -> Option<Diagnosis> {
    let management = draft.management.as_ref()?;
    let Some(path) = token_file_or(management, environment.get(TOKEN_FILE_ENV)) else {
        return Some(
            Diagnosis::critical(
                Check::Files,
                "management",
                "no token file is configured, so the management API cannot be served",
            )
            .fixed_by(
                "set management.token_file, or OXIROUTE_MANAGEMENT_TOKEN_FILE for the daemon",
            ),
        );
    };
    let subject = path.display().to_string();
    Some(match preflight_management_token(&path) {
        Ok(()) => Diagnosis::ok(
            Check::Files,
            subject,
            "the management token file is a regular file with mode 0400 or 0600",
        ),
        Err(error) => {
            let fix = match mode_of(&path) {
                Ok(_) => format!("chmod 600 {subject}"),
                Err(_) => format!("create {subject} with the token and mode 0600"),
            };
            Diagnosis::critical(Check::Files, subject, error.to_string()).fixed_by(fix)
        }
    })
}

pub(super) fn of_config(draft: &ConfigDraft, environment: &Environment) -> Vec<Diagnosis> {
    let secrets = secrets(draft);
    let mut diagnoses = management_token(draft, environment)
        .into_iter()
        .collect::<Vec<_>>();
    let problems = secrets
        .iter()
        .filter_map(|(path, secret)| judge(path, secret))
        .collect::<Vec<_>>();
    if problems.is_empty() && !secrets.is_empty() {
        diagnoses.push(Diagnosis::ok(
            Check::Files,
            "secrets",
            format!("the {} private keys, credentials and token files the configuration names are not open to other users", secrets.len()),
        ));
    }
    diagnoses.extend(problems);
    diagnoses
}
