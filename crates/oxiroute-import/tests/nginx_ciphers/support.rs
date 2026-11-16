use std::{fs, path::Path};

use oxiroute_config::{TlsPolicy, TlsProfile};
use oxiroute_import::{
    Diagnostic, Severity,
    nginx::{ImportReport, import_http_fragment},
};

pub const DHPARAM: &str = "/etc/nginx/dhparam.pem";

pub fn import(directives: &str) -> ImportReport {
    import_with_protocols(directives, "TLSv1.2 TLSv1.3")
}

pub fn import_with_protocols(directives: &str, protocols: &str) -> ImportReport {
    let directory = tempfile::tempdir().expect("create source directory");
    let source = format!(
        "http {{ access_log off; ssl_protocols {protocols}; {directives}
          server {{
            listen 127.0.0.1:8443 ssl default_server;
            server_name host-a.example;
            ssl_certificate /definitely/missing/fullchain.pem;
            ssl_certificate_key /definitely/missing/privkey.pem;
            location / {{ return 204; }}
          }}
        }}"
    );
    fs::write(directory.path().join("nginx.conf"), source).expect("write source");
    import_http_fragment(Path::new("nginx.conf"), directory.path())
}

pub fn profile(report: &ImportReport) -> TlsProfile {
    assert!(
        report.blocked_services.is_empty(),
        "{:?}",
        report.diagnostics
    );
    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("imported nginx TLS config");
    config.tls_profiles[0].clone()
}

pub fn policy(report: &ImportReport) -> TlsPolicy {
    profile(report).policy
}

pub fn warnings(report: &ImportReport) -> Vec<&Diagnostic> {
    report
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity() == Severity::Warning)
        .collect()
}

pub fn warning_codes(report: &ImportReport) -> Vec<&'static str> {
    warnings(report)
        .iter()
        .map(|diagnostic| diagnostic.code().as_str())
        .collect()
}
