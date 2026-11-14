use crate::{
    Diagnostic, DiagnosticCode, E_INVALID_VALUE, E_SEMANTICS_NOT_REPRESENTABLE,
    E_UNSUPPORTED_FEATURE,
};

use crate::nginx::{EffectiveHttp, EffectiveServer, OccurrenceDisposition};

use super::{
    LowerIssue, Lowerer,
    provenance::{issue, utf8, warning},
};

/// nginx buffers proxied responses by default; the runtime streams them.
pub(super) const W_PROXY_BUFFERING_STREAMED: DiagnosticCode =
    DiagnosticCode::new("W_PROXY_BUFFERING_STREAMED");
/// An https origin is lowered the way nginx treats it by default: encrypted but not authenticated.
pub(super) const W_UPSTREAM_TLS_UNVERIFIED: DiagnosticCode =
    DiagnosticCode::new(oxiroute_config::W_UPSTREAM_TLS_UNVERIFIED);
/// A text access log is replaced by the structured JSON access log, which records fewer fields.
const W_ACCESS_LOG_MIGRATED: DiagnosticCode = DiagnosticCode::new("W_ACCESS_LOG_MIGRATED");
/// A directive is accepted but changes nothing at runtime.
const W_DIRECTIVE_NO_EFFECT: DiagnosticCode = DiagnosticCode::new("W_DIRECTIVE_NO_EFFECT");

const COMBINED_FORMAT: &str = "$remote_addr - $remote_user [$time_local] \"$request\" $status $body_bytes_sent \"$http_referer\" \"$http_user_agent\"";
/// Variables the structured access log records, in its own field encoding.
const REPRODUCED_VARIABLES: [&str; 10] = [
    "host",
    "status",
    "request_method",
    "body_bytes_sent",
    "bytes_sent",
    "request_time",
    "msec",
    "time_local",
    "time_iso8601",
    "request_id",
];
const ACCESS_LOG_OPTION_PREFIXES: [&[u8]; 4] = [b"buffer=", b"gzip", b"flush=", b"if="];

const NO_EFFECT: [(&[u8], &str); 6] = [
    (
        b"send_timeout",
        "send_timeout has no runtime equivalent; the runtime bounds a request with its own client timeout",
    ),
    (
        b"large_client_header_buffers",
        "large_client_header_buffers is not applied; the runtime enforces its own request header limits",
    ),
    (
        b"sendfile",
        "sendfile is not applied; the runtime serves files through its own path",
    ),
    (
        b"types_hash_max_size",
        "types_hash_max_size tunes an nginx internal table and has no runtime equivalent",
    ),
    (
        b"limit_conn_zone",
        "limit_conn_zone declares a zone that no imported directive uses, so no connection limit applies",
    ),
    (
        b"limit_req_zone",
        "limit_req_zone declares a zone that no imported directive uses, so no rate limit applies",
    ),
];

pub(super) enum AccessLog {
    Disabled,
    Migrated,
    Rejected,
}

fn format_variables(format: &[u8]) -> Vec<String> {
    let mut variables = Vec::new();
    let mut rest = format;
    while let Some(dollar) = rest.iter().position(|byte| *byte == b'$') {
        rest = &rest[dollar + 1..];
        let braced = rest.first() == Some(&b'{');
        let body = if braced { &rest[1..] } else { rest };
        let length = body
            .iter()
            .take_while(|byte| byte.is_ascii_alphanumeric() || **byte == b'_')
            .count();
        if length > 0 {
            let name = String::from_utf8_lossy(&body[..length]).into_owned();
            if !variables.contains(&name) {
                variables.push(name);
            }
        }
        rest = &body[length..];
    }
    variables
}

fn dropped_fields(format: &str) -> String {
    format_variables(format.as_bytes())
        .into_iter()
        .filter(|name| !REPRODUCED_VARIABLES.contains(&name.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}

impl Lowerer {
    /// Decides how one server's nginx access logging maps to the single structured log of its service.
    pub(super) fn server_access_log(
        &self,
        http: &EffectiveHttp,
        server: &EffectiveServer,
        warnings: &mut Vec<Diagnostic>,
        issues: &mut Vec<LowerIssue>,
    ) -> AccessLog {
        let entries = self.effective_list_policy_chain(server.origin.occurrence, b"access_log");
        if entries.is_empty() {
            return self.migrate_access_log(http, &http.origin, &[], warnings, issues);
        }
        let logging = entries
            .iter()
            .filter(|entry| entry.arguments.first().is_none_or(|value| value != b"off"))
            .collect::<Vec<_>>();
        if logging.is_empty() {
            return AccessLog::Disabled;
        }
        let mut outcome = AccessLog::Migrated;
        for entry in logging {
            let origin = entry.origins.last().unwrap_or(&server.origin);
            if let AccessLog::Rejected =
                self.migrate_access_log(http, origin, &entry.arguments, warnings, issues)
            {
                outcome = AccessLog::Rejected;
            }
        }
        outcome
    }

    /// `arguments` are those of one `access_log` directive; empty means nginx's implicit combined log.
    fn migrate_access_log(
        &self,
        http: &EffectiveHttp,
        origin: &crate::nginx::DirectiveOrigin,
        arguments: &[Vec<u8>],
        warnings: &mut Vec<Diagnostic>,
        issues: &mut Vec<LowerIssue>,
    ) -> AccessLog {
        let omitted = arguments.is_empty();
        if omitted && self.default_access_log_path.is_none() {
            issues.push(issue(
                origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "omitted nginx access_log enables the unrepresented default combined log",
            ));
            return AccessLog::Rejected;
        }
        if self.default_access_log_path.is_none() {
            issues.push(issue(
                origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "nginx formatted access_log output is not equivalent to canonical JSON access logging without a structured access-log migration overlay",
            ));
            return AccessLog::Rejected;
        }
        let target = arguments.first().map(Vec::as_slice).unwrap_or_default();
        if target.starts_with(b"syslog:") || target.contains(&b'$') {
            issues.push(issue(
                origin,
                E_UNSUPPORTED_FEATURE,
                "nginx access_log target is not a static file",
            ));
            return AccessLog::Rejected;
        }
        let options = arguments.get(1..).unwrap_or_default();
        let named = options.first().filter(|value| {
            !ACCESS_LOG_OPTION_PREFIXES
                .iter()
                .any(|prefix| value.starts_with(prefix))
        });
        let (name, format) = if let Some(name) = named {
            let Some(format) = self.log_format(http, name) else {
                issues.push(issue(
                    origin,
                    E_INVALID_VALUE,
                    "nginx access_log references a log_format that is not defined in the http context",
                ));
                return AccessLog::Rejected;
            };
            (String::from_utf8_lossy(name).into_owned(), format)
        } else {
            ("combined".to_owned(), COMBINED_FORMAT.to_owned())
        };
        let mut message = format!(
            "nginx access_log format `{name}` migrates to the structured JSON access log; fields not reproduced: {}",
            dropped_fields(&format)
        );
        if options.iter().any(|option| option.starts_with(b"if=")) {
            message.push_str("; the if= condition is dropped, so every request is logged");
        }
        warnings.push(warning(origin, W_ACCESS_LOG_MIGRATED, message));
        AccessLog::Migrated
    }

    fn log_format(&self, http: &EffectiveHttp, name: &[u8]) -> Option<String> {
        if name == b"combined" {
            return Some(COMBINED_FORMAT.to_owned());
        }
        let definition = self
            .effective_list_policy_chain(http.origin.occurrence, b"log_format")
            .into_iter()
            .find(|policy| policy.arguments.first().is_some_and(|value| value == name))?;
        self.used_log_formats
            .borrow_mut()
            .extend(definition.origins.iter().map(|origin| origin.occurrence));
        let parts = definition.arguments[1..]
            .iter()
            .filter(|part| !part.starts_with(b"escape="))
            .map(|part| utf8(part).unwrap_or_default())
            .collect::<String>();
        Some(parts)
    }

    /// Warnings for resolved directives that the runtime accepts but does not act on.
    pub(super) fn no_effect_warnings(&self) -> Vec<Diagnostic> {
        let used_formats = self.used_log_formats.borrow();
        self.resolution
            .decisions
            .iter()
            .filter(|decision| decision.disposition == OccurrenceDisposition::Resolved)
            .filter_map(|decision| {
                let name = decision.name.value.as_slice();
                let message = match name {
                    b"error_log" => format!(
                        "error_log {} in the http context has no runtime effect",
                        if decision
                            .arguments
                            .first()
                            .is_some_and(|value| value.value == b"off")
                        {
                            "off (nginx writes a file named off)"
                        } else {
                            "target"
                        }
                    ),
                    b"log_format" if !used_formats.contains(&decision.occurrence) => {
                        "log_format is not referenced by a migrated access_log".to_owned()
                    }
                    _ => NO_EFFECT
                        .iter()
                        .find(|(directive, _)| *directive == name)?
                        .1
                        .to_owned(),
                };
                Some(warning(
                    &self.origin(decision.occurrence),
                    W_DIRECTIVE_NO_EFFECT,
                    message,
                ))
            })
            .collect()
    }
}
