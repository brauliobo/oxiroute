use std::{
    fs,
    path::{Path, PathBuf},
};

use oxiroute_config::{
    AccessLogPolicy, CacheStore, CertificateSource, ConfigDraft, ListenerBind, RegistryEndpoint,
};

use super::{
    diagnosis::{Check, Diagnosis},
    unit::{Access, Environment, Unit},
};
use crate::{
    observability::{operational_event::audit_directory, status::summary::Severity},
    rtmp::api::{TOKEN_FILE_ENV, token_file_or},
};

pub(super) struct WritePath {
    what: String,
    path: PathBuf,
    directory: bool,
    severity: Severity,
}

impl WritePath {
    fn directory(what: impl Into<String>, path: &Path) -> Self {
        Self {
            what: what.into(),
            path: path.to_owned(),
            directory: true,
            severity: Severity::Critical,
        }
    }

    fn file(what: impl Into<String>, path: &Path) -> Self {
        Self {
            what: what.into(),
            path: path.to_owned(),
            directory: false,
            severity: Severity::Critical,
        }
    }

    fn degrading(mut self) -> Self {
        self.severity = Severity::Warning;
        self
    }

    fn grant(&self, absolute: &Path) -> PathBuf {
        if self.directory {
            absolute.to_owned()
        } else {
            absolute.parent().unwrap_or(absolute).to_owned()
        }
    }
}

pub(super) fn of(draft: &ConfigDraft, environment: &Environment, config: &Path) -> Vec<WritePath> {
    let mut paths = beside_config(draft, config);
    for certificate in &draft.certificates {
        if let CertificateSource::AcmeManaged { state_root, .. } = &certificate.source {
            let what = format!("state of managed certificate {}", certificate.name);
            paths.push(WritePath::directory(what, state_root));
        }
    }
    for store in &draft.cache_stores {
        if let CacheStore::Disk {
            name,
            root_directory,
            ..
        } = store
        {
            paths.push(WritePath::directory(
                format!("disk cache {name}"),
                root_directory,
            ));
        }
    }
    paths.extend(audit(draft, environment));
    if let Some(registry) = &draft.registry {
        paths.push(WritePath::directory("registry state", &registry.state_dir));
        for endpoint in &registry.endpoints {
            let RegistryEndpoint::Unix { path, .. } = endpoint else {
                continue;
            };
            paths.push(WritePath::file("registry socket", path));
        }
    }
    for listener in &draft.listeners {
        if let ListenerBind::Unix { path, .. } = &listener.bind {
            paths.push(WritePath::file(
                format!("socket of listener {}", listener.name),
                path,
            ));
        }
    }
    paths.extend(access_logs(draft));
    paths.extend(rtmp(draft));
    paths
}

fn beside_config(draft: &ConfigDraft, config: &Path) -> Vec<WritePath> {
    if draft.management.is_none() {
        return Vec::new();
    }
    let canonical = fs::canonicalize(config).unwrap_or_else(|_| config.to_owned());
    let directory = canonical.parent().unwrap_or(&canonical);
    let what = "configuration directory (config apply, --persist overlays and history)";
    vec![WritePath::directory(what, directory).degrading()]
}

fn audit(draft: &ConfigDraft, environment: &Environment) -> Option<WritePath> {
    let token = draft
        .management
        .as_ref()
        .and_then(|management| token_file_or(management, environment.get(TOKEN_FILE_ENV)));
    let directory = audit_directory(draft.audit.as_ref(), token.as_deref(), |name| {
        environment.get(name)
    })?;
    Some(WritePath::directory("audit log", &directory))
}

fn access_logs(draft: &ConfigDraft) -> Vec<WritePath> {
    let services = draft
        .http_services
        .iter()
        .map(|service| (&service.name, &service.access_log))
        .chain(
            draft
                .rtmp_services
                .iter()
                .map(|service| (&service.name, &service.access_log)),
        );
    services
        .filter_map(|(name, log)| match log {
            Some(AccessLogPolicy::File { path }) => {
                Some(WritePath::file(format!("access log of {name}"), path))
            }
            _ => None,
        })
        .collect()
}

fn rtmp(draft: &ConfigDraft) -> Vec<WritePath> {
    let mut paths = Vec::new();
    for service in &draft.rtmp_services {
        if service.auto_push.enabled {
            let what = format!("auto push sockets of {}", service.name);
            paths.push(WritePath::directory(what, &service.auto_push.socket_dir));
        }
        for application in &service.applications {
            let name = format!("{}/{}", service.name, application.name);
            let outputs = [
                (
                    "HLS output",
                    application.hls.as_ref().map(|hls| &hls.root_directory),
                ),
                (
                    "DASH output",
                    application.dash.as_ref().map(|dash| &dash.root_directory),
                ),
            ];
            for (what, root) in outputs {
                paths.extend(
                    root.map(|root| WritePath::directory(format!("{what} of {name}"), root)),
                );
            }
            for recorder in &application.recorders {
                let what = format!("recorder {} of {name}", recorder.name);
                paths.push(WritePath::directory(what, &recorder.root_directory));
            }
        }
    }
    paths
}

pub(super) fn judge(unit: &Unit, writes: &[WritePath]) -> Vec<Diagnosis> {
    let subject = unit.path.display().to_string();
    let mut diagnoses = unit
        .missing_required()
        .map(|path| {
            Diagnosis::critical(
                Check::Unit,
                &subject,
                format!(
                    "ReadWritePaths= names {}, which does not exist, so the unit cannot start",
                    path.display()
                ),
            )
            .fixed_by(format!(
                "create {} or write -{} to make it optional",
                path.display(),
                path.display()
            ))
        })
        .collect::<Vec<_>>();
    let denied = writes
        .iter()
        .filter_map(|write| {
            let absolute = unit.absolute(&write.path);
            let state = match unit.access(&absolute) {
                Access::Writable => return None,
                Access::ReadOnly => "read-only",
                Access::Hidden => "inaccessible",
            };
            Some(
                Diagnosis::new(
                    write.severity,
                    Check::Unit,
                    &subject,
                    format!("{} {} is {state} under the unit's sandbox", write.what, absolute.display()),
                )
                .fixed_by(format!(
                    "add ReadWritePaths={} to the unit (systemctl edit oxiroute.service) and restart it",
                    write.grant(&absolute).display()
                )),
            )
        })
        .collect::<Vec<_>>();
    if denied.is_empty() {
        let message = match writes.len() {
            0 => "the configuration writes no files the unit could block".to_owned(),
            count => {
                format!("the {count} paths the configuration writes are writable under the unit")
            }
        };
        diagnoses.push(Diagnosis::ok(Check::Unit, &subject, message));
    }
    diagnoses.extend(denied);
    diagnoses
}
