use std::{collections::HashMap, path::Path};

use oxiroute_config::{Config, ListenerBind, UpstreamTls, validate_config};

use crate::{
    CanonicalDraft, CanonicalProvenance, Diagnostic, DiagnosticCode, DiagnosticStage,
    E_INVALID_VALUE, Report, Severity,
};

use super::{Lowerer, provenance::lower_diagnostic};
use crate::nginx::{OccurrenceDecision, OccurrenceId, SourceGraph, load};

/// One native bind service that cannot be translated safely.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockedService {
    pub path: String,
    pub bind: Option<ListenerBind>,
    pub servers: Vec<OccurrenceId>,
    pub diagnostic_codes: Vec<DiagnosticCode>,
}

/// Complete nginx HTTP parsing, resolution, and blocked-service evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportReport {
    pub source_graph: SourceGraph,
    pub occurrence_ledger: Vec<OccurrenceDecision>,
    pub diagnostics: Vec<Diagnostic>,
    pub blocked_services: Vec<BlockedService>,
    pub draft: CanonicalDraft,
    pub provenance: Vec<CanonicalProvenance<crate::nginx::DirectiveOrigin>>,
    pub config: Option<Config>,
    pub(crate) used_upstream_tls_overlays: std::collections::HashSet<Vec<u8>>,
    pub(crate) used_bearer_token_overlays: std::collections::HashSet<Vec<u8>>,
    pub(crate) used_certificate_overlays: std::collections::HashSet<OccurrenceId>,
    pub(crate) used_htpasswd_overlays: std::collections::HashSet<OccurrenceId>,
    pub(crate) used_default_access_log_overlay: bool,
    pub(crate) used_default_error_overlay: bool,
}

impl ImportReport {
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity() == Severity::Error)
    }
}

/// Loads, resolves, and audits an nginx fragment whose expanded root contains only `http`.
#[must_use]
pub fn import_http_fragment(root: &Path, root_prefix: &Path) -> ImportReport {
    lower_http(load(root, root_prefix))
}

pub(super) fn lower_http(loaded: Report<SourceGraph>) -> ImportReport {
    lower_http_with_mode(loaded, false, HashMap::new(), HashMap::new(), None, None)
}

pub(crate) fn lower_http_root_with_overlays(
    loaded: Report<SourceGraph>,
    upstream_tls_overlays: HashMap<Vec<u8>, UpstreamTls>,
    bearer_token_overlays: HashMap<Vec<u8>, std::path::PathBuf>,
    default_access_log_path: Option<std::path::PathBuf>,
    default_error_server: Option<String>,
) -> ImportReport {
    lower_http_with_mode(
        loaded,
        true,
        upstream_tls_overlays,
        bearer_token_overlays,
        default_access_log_path,
        default_error_server,
    )
}

fn lower_http_with_mode(
    loaded: Report<SourceGraph>,
    complete_root: bool,
    upstream_tls_overlays: HashMap<Vec<u8>, UpstreamTls>,
    bearer_token_overlays: HashMap<Vec<u8>, std::path::PathBuf>,
    default_access_log_path: Option<std::path::PathBuf>,
    default_error_server: Option<String>,
) -> ImportReport {
    let (graph, mut diagnostics) = loaded.into_parts();
    let resolved = if complete_root {
        crate::nginx::semantic::resolve_http_root_graph(&graph)
    } else {
        crate::nginx::semantic::resolve_http_graph(&graph)
    };
    let (resolution, resolve_diagnostics) = resolved.into_parts();
    diagnostics.extend(resolve_diagnostics);
    Lowerer::new(
        graph,
        resolution,
        diagnostics,
        upstream_tls_overlays,
        bearer_token_overlays,
        default_access_log_path,
        default_error_server,
    )
    .run()
}

impl Lowerer {
    fn run(mut self) -> ImportReport {
        let http_blocks = self.resolution.http_blocks.clone();
        for (http_index, http) in http_blocks.iter().enumerate() {
            for (bind_index, bind) in http.binds.iter().enumerate() {
                let native_path = format!("/nginx/http/{http_index}/binds/{bind_index}");
                let block = self.lower_bind(http, bind, http_index, bind_index);
                self.commit_block(native_path, bind, block);
            }
        }

        let draft = self.draft.clone();
        let config = self.finalize(&draft);
        let used_upstream_tls_overlays = self.used_upstream_tls_overlays.into_inner();
        let used_bearer_token_overlays = self.used_bearer_token_overlays.into_inner();
        let used_certificate_overlays = self.used_certificate_overlays.into_inner();
        let used_htpasswd_overlays = self.used_htpasswd_overlays.into_inner();
        let used_default_access_log_overlay = self.used_default_access_log_overlay;
        let used_default_error_overlay = self.used_default_error_overlay.into_inner();
        let ((), diagnostics) = Report::new((), self.diagnostics).into_parts();
        ImportReport {
            source_graph: self.graph,
            occurrence_ledger: self.resolution.decisions,
            diagnostics,
            blocked_services: self.blocked_services,
            draft,
            provenance: self.provenance,
            config,
            used_upstream_tls_overlays,
            used_bearer_token_overlays,
            used_certificate_overlays,
            used_htpasswd_overlays,
            used_default_access_log_overlay,
            used_default_error_overlay,
        }
    }

    fn commit_block(
        &mut self,
        path: String,
        bind: &crate::nginx::EffectiveBind,
        block: super::BindBlock,
    ) {
        if block.issues.is_empty() {
            if let Some(candidate) = block.candidate {
                self.commit_candidate(candidate);
            }
            return;
        }
        let mut diagnostic_codes = Vec::new();
        for issue in block.issues {
            if !diagnostic_codes.contains(&issue.code) {
                diagnostic_codes.push(issue.code);
            }
            if issue.emit {
                self.diagnostics.push(lower_diagnostic(&issue));
            }
        }
        self.blocked_services.push(BlockedService {
            path,
            bind: block.bind,
            servers: bind.servers.clone(),
            diagnostic_codes,
        });
    }

    fn finalize(&mut self, draft: &CanonicalDraft) -> Option<Config> {
        if self
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity() == Severity::Error)
        {
            return None;
        }
        let mut config = draft.to_config();
        if let Err(error) = validate_config(&mut config) {
            let mut diagnostic = Diagnostic::new(
                E_INVALID_VALUE,
                Severity::Error,
                DiagnosticStage::Validate,
                format!("lowered nginx canonical draft is invalid: {error}"),
            );
            if let Some(origin) = self
                .provenance
                .first()
                .and_then(|provenance| provenance.origins.first())
            {
                diagnostic = diagnostic.with_primary_span(origin.span);
            }
            self.diagnostics.push(diagnostic);
            return None;
        }
        Some(config)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "canonical object insertion and complete provenance are committed atomically"
    )]
    fn commit_candidate(&mut self, candidate: super::BindCandidate) {
        for certificate in candidate.certificates {
            if let Some(index) = self
                .draft
                .certificates
                .iter()
                .position(|existing| existing.name == certificate.name)
            {
                let existing = &mut self.draft.certificates[index];
                for dns_name in certificate.dns_names {
                    if !existing.dns_names.contains(&dns_name) {
                        existing.dns_names.push(dns_name);
                    }
                }
                self.record(
                    format!("/certificates/{index}/dns_names"),
                    candidate.origins.clone(),
                );
                continue;
            }
            let index = self.draft.certificates.len();
            self.draft.certificates.push(certificate);
            let path = format!("/certificates/{index}");
            for suffix in [
                "",
                "/name",
                "/dns_names",
                "/source",
                "/source/type",
                "/source/certificate_chain_path",
                "/source/private_key_path",
            ] {
                self.record(format!("{path}{suffix}"), candidate.origins.clone());
            }
        }
        if let Some(profile) = candidate.tls_profile {
            let index = self.draft.tls_profiles.len();
            self.draft.tls_profiles.push(profile);
            let path = format!("/tls_profiles/{index}");
            for suffix in [
                "",
                "/name",
                "/certificates",
                "/default_certificate",
                "/min_version",
                "/alpn",
            ] {
                self.record(format!("{path}{suffix}"), candidate.origins.clone());
            }
        }
        for pool in candidate.pools {
            if self
                .draft
                .upstream_pools
                .iter()
                .any(|existing| existing.name == pool.pool.name)
            {
                continue;
            }
            let index = self.draft.upstream_pools.len();
            self.draft.upstream_pools.push(pool.pool);
            self.record_pool_provenance(index, &pool.origin, pool.endpoint_origins);
        }
        let service_index = self.draft.http_services.len();
        self.draft.http_services.push(candidate.service);
        let service_path = format!("/http_services/{service_index}");
        self.record(service_path.clone(), candidate.origins.clone());
        self.record(
            format!("{service_path}/upstream_io_timeout_ms"),
            candidate.origins.clone(),
        );
        self.record(
            format!("{service_path}/max_request_body_bytes"),
            candidate.origins.clone(),
        );
        for (route_index, origins) in candidate.route_origins.into_iter().enumerate() {
            let path = format!("{service_path}/routes/{route_index}");
            let route = self.draft.http_services[service_index].routes[route_index].clone();
            let mut suffixes = vec![
                "",
                "/path",
                "/path/kind",
                "/path/value",
                "/methods",
                "/action",
                "/action/type",
            ];
            if route.host.is_some() {
                suffixes.extend(["/host", "/host/kind", "/host/value"]);
            }
            if route.access_policy.is_some() {
                suffixes.push("/access_policy");
            }
            match &route.action {
                oxiroute_config::HttpRouteAction::Proxy { .. } => suffixes.extend([
                    "/action/upstream_pool",
                    "/action/policy",
                    "/action/policy/upstream_host",
                    "/action/policy/upstream_host/type",
                    "/action/policy/request_headers",
                    "/action/policy/response_headers",
                    "/action/policy/response_cookie_path_rewrites",
                    "/action/policy/retry",
                    "/action/policy/retry/max_retries",
                    "/action/policy/retry/triggers",
                    "/action/policy/retry/method_safety",
                    "/action/policy/retry/body_safety",
                ]),
                oxiroute_config::HttpRouteAction::FixedResponse { .. } => {
                    suffixes.extend(["/action/status", "/action/body", "/action/headers"]);
                }
                oxiroute_config::HttpRouteAction::Redirect { .. } => {
                    suffixes.extend(["/action/status", "/action/location"]);
                }
                oxiroute_config::HttpRouteAction::StaticFiles { .. } => {
                    suffixes.extend(["/action/root_directory", "/action/index_files"]);
                }
            }
            for suffix in suffixes {
                self.record(format!("{path}{suffix}"), origins.clone());
            }
            self.record_route_action_items(&path, &route, &origins);
        }
        let listener_index = self.draft.listeners.len();
        self.draft.listeners.push(candidate.listener);
        let listener_path = format!("/listeners/{listener_index}");
        let listener = self.draft.listeners[listener_index].clone();
        let mut suffixes = vec!["", "/name", "/bind", "/bind/type", "/protocol", "/service"];
        match listener.bind {
            ListenerBind::Socket { .. } | ListenerBind::Udp { .. } => {
                suffixes.push("/bind/address");
            }
            ListenerBind::Unix { .. } => suffixes.push("/bind/path"),
        }
        if listener.tls_profile.is_some() {
            suffixes.push("/tls_profile");
        }
        for suffix in suffixes {
            self.record(
                format!("{listener_path}{suffix}"),
                candidate.origins.clone(),
            );
        }
    }

    fn record_pool_provenance(
        &mut self,
        index: usize,
        origin: &crate::nginx::DirectiveOrigin,
        endpoint_origins: Vec<crate::nginx::DirectiveOrigin>,
    ) {
        let path = format!("/upstream_pools/{index}");
        for suffix in ["", "/name", "/algorithm", "/http_versions"] {
            self.record(format!("{path}{suffix}"), vec![origin.clone()]);
        }
        self.record(format!("{path}/servers"), endpoint_origins.clone());
        let servers = self.draft.upstream_pools[index].servers.clone();
        for (endpoint_index, (server, origin)) in
            servers.into_iter().zip(endpoint_origins).enumerate()
        {
            let server_path = format!("{path}/servers/{endpoint_index}");
            self.record(server_path.clone(), vec![origin.clone()]);
            self.record(format!("{server_path}/name"), vec![origin.clone()]);
            let endpoint_path = format!("{server_path}/endpoint");
            let suffixes: &[&str] = match server.endpoint {
                oxiroute_config::UpstreamEndpoint::Socket { .. } => &["", "/type", "/address"],
                oxiroute_config::UpstreamEndpoint::Dns { .. } => &["", "/type", "/host", "/port"],
                oxiroute_config::UpstreamEndpoint::Unix { .. } => &["", "/type", "/path"],
            };
            for suffix in suffixes {
                self.record(format!("{endpoint_path}{suffix}"), vec![origin.clone()]);
            }
        }
    }

    fn record_route_action_items(
        &mut self,
        route_path: &str,
        route: &oxiroute_config::HttpRoute,
        origins: &[crate::nginx::DirectiveOrigin],
    ) {
        match &route.action {
            oxiroute_config::HttpRouteAction::Proxy { policy, .. } => {
                self.record_proxy_policy_items(route_path, policy, origins);
            }
            oxiroute_config::HttpRouteAction::FixedResponse { headers, .. } => {
                for index in 0..headers.len() {
                    let path = format!("{route_path}/action/headers/{index}");
                    for suffix in ["", "/name", "/value"] {
                        self.record(format!("{path}{suffix}"), origins.to_vec());
                    }
                }
            }
            oxiroute_config::HttpRouteAction::Redirect { .. } => {
                self.record(
                    format!("{route_path}/action/location/type"),
                    origins.to_vec(),
                );
                self.record(
                    format!("{route_path}/action/location/value"),
                    origins.to_vec(),
                );
            }
            oxiroute_config::HttpRouteAction::StaticFiles { spa_fallback, .. } => {
                if spa_fallback.is_some() {
                    self.record(
                        format!("{route_path}/action/spa_fallback"),
                        origins.to_vec(),
                    );
                }
            }
        }
    }

    fn record_proxy_policy_items(
        &mut self,
        route_path: &str,
        policy: &oxiroute_config::HttpProxyPolicy,
        origins: &[crate::nginx::DirectiveOrigin],
    ) {
        let policy_path = format!("{route_path}/action/policy");
        match &policy.upstream_host {
            oxiroute_config::HttpUpstreamHost::PreserveIncoming => {}
            oxiroute_config::HttpUpstreamHost::NginxHost { .. } => self.record(
                format!("{policy_path}/upstream_host/fallback"),
                origins.to_vec(),
            ),
            oxiroute_config::HttpUpstreamHost::Endpoint { unix_fallback } => {
                if unix_fallback.is_some() {
                    self.record(
                        format!("{policy_path}/upstream_host/unix_fallback"),
                        origins.to_vec(),
                    );
                }
            }
            oxiroute_config::HttpUpstreamHost::Literal { .. } => self.record(
                format!("{policy_path}/upstream_host/value"),
                origins.to_vec(),
            ),
        }
        for (index, mutation) in policy.request_headers.iter().enumerate() {
            let path = format!("{policy_path}/request_headers/{index}");
            for suffix in ["", "/operation", "/name"] {
                self.record(format!("{path}{suffix}"), origins.to_vec());
            }
            if let oxiroute_config::HttpRequestHeaderMutation::Set { value, .. } = mutation {
                self.record(format!("{path}/value"), origins.to_vec());
                self.record(format!("{path}/value/type"), origins.to_vec());
                if matches!(
                    value,
                    oxiroute_config::HttpRequestHeaderValue::Literal { .. }
                ) {
                    self.record(format!("{path}/value/value"), origins.to_vec());
                }
            }
        }
        for (index, mutation) in policy.response_headers.iter().enumerate() {
            let path = format!("{policy_path}/response_headers/{index}");
            for suffix in ["", "/operation", "/name"] {
                self.record(format!("{path}{suffix}"), origins.to_vec());
            }
            if matches!(
                mutation,
                oxiroute_config::HttpResponseHeaderMutation::Set { .. }
                    | oxiroute_config::HttpResponseHeaderMutation::Add { .. }
            ) {
                self.record(format!("{path}/value"), origins.to_vec());
                self.record(format!("{path}/always"), origins.to_vec());
            }
        }
        for index in 0..policy.response_cookie_path_rewrites.len() {
            let path = format!("{policy_path}/response_cookie_path_rewrites/{index}");
            for suffix in ["", "/from", "/to"] {
                self.record(format!("{path}{suffix}"), origins.to_vec());
            }
        }
        for index in 0..policy.retry.triggers.len() {
            self.record(
                format!("{policy_path}/retry/triggers/{index}"),
                origins.to_vec(),
            );
        }
    }

    fn record(&mut self, path: String, mut origins: Vec<crate::nginx::DirectiveOrigin>) {
        origins.sort_unstable_by_key(|origin| origin.occurrence);
        origins.dedup_by_key(|origin| origin.occurrence);
        if origins.is_empty() {
            return;
        }
        if let Some(existing) = self
            .provenance
            .iter_mut()
            .find(|provenance| provenance.path == path)
        {
            existing.origins.extend(origins);
            existing
                .origins
                .sort_unstable_by_key(|origin| origin.occurrence);
            existing.origins.dedup_by_key(|origin| origin.occurrence);
        } else {
            self.provenance.push(CanonicalProvenance { path, origins });
        }
    }
}
