use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};

use http::{HeaderName, HeaderValue};
use oxiroute_config::{
    CacheKeyComponent, CacheStore, Config, DnsResolutionPolicy, DownstreamTimeoutPolicy,
    HttpCachePolicy, HttpProxyPolicy, HttpRequestHeaderMutation, HttpRequestHeaderValue,
    HttpResponseHeaderMutation, HttpRoute, HttpRouteAction, HttpRoutePolicy, HttpService,
    HttpUpstreamHost, Listener, ListenerBind, Protocol, UpstreamAlgorithm, UpstreamConnectionReuse,
    UpstreamEndpoint, UpstreamPool, UpstreamServer, validate_config,
};

use crate::{
    CanonicalCandidate, CanonicalDraft, CanonicalProvenance, Diagnostic, DiagnosticStage,
    E_INVALID_VALUE, E_SOURCE_CHANGED, E_SOURCE_LIMIT, E_UNSUPPORTED_FEATURE, Severity,
    SourceImportMetadata,
};

use super::{
    Backend, BackendKind, BackendProperty, BackendReference, CacheFlag, CacheLifetimeField,
    CompressionOperation, Director, DirectorKind, E_VCL_LOWERING_BLOCKED, E_VCL_SEMANTIC_MISMATCH,
    E_VCL_UNSUPPORTED_SUBROUTINE, Expression, ExpressionKind, FeatureBehavior, FlowAction,
    HeaderMutation, HeaderOperation, HeaderScope, InvocationFacts, LoweringBlocker, LoweringStatus,
    ModernDirector, Provenance, SourceGraph, StatementClassification, StatementDecision,
    Subroutine, SubroutineKind, VmodImport, VmodObject,
};

/// Stable capability profile used by Varnish reports and native source references.
pub const VARNISH_CAPABILITY_PROFILE_ID: &str = "varnish-vcl-exact-cache";
pub const VARNISH_CAPABILITY_PROFILE_VERSION: u32 = 1;

/// Canonical candidate produced by the exact Varnish lowering pass.
pub type VarnishCanonicalCandidate = CanonicalCandidate<Provenance>;

const DEFAULT_LISTEN: SocketAddr =
    SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 6081);
const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 3_500;
const DEFAULT_FIRST_BYTE_TIMEOUT_MS: u64 = 60_000;
const DEFAULT_BETWEEN_BYTES_TIMEOUT_MS: u64 = 60_000;
const DEFAULT_TTL_MS: u64 = 120_000;
const DEFAULT_GRACE_MS: u64 = 10_000;
const DEFAULT_KEEP_MS: u64 = 0;
const DEFAULT_MEMORY_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_MEMORY_ENTRIES: u64 = 100_000;
const DEFAULT_DISK_FILES: u64 = 1_000_000;
const DEFAULT_OBJECT_BYTES: u64 = 16 * 1024 * 1024;
const DEFAULT_HEADER_BYTES: u64 = 64 * 1024;
const DEFAULT_KEY_BYTES: u64 = 4 * 1024;
const DEFAULT_TAG_BYTES: u64 = 256;
const DEFAULT_TAGS_PER_OBJECT: u64 = 64;
const DEFAULT_IN_FLIGHT_FILLS: u64 = 1_024;
const DEFAULT_FOLLOWERS_PER_FILL: u64 = 128;

#[derive(Clone, Copy)]
struct RouteTiming {
    connect_ms: u64,
    read_ms: u64,
    write_ms: u64,
}

impl Default for RouteTiming {
    fn default() -> Self {
        Self {
            connect_ms: DEFAULT_CONNECT_TIMEOUT_MS,
            read_ms: DEFAULT_FIRST_BYTE_TIMEOUT_MS,
            write_ms: DEFAULT_BETWEEN_BYTES_TIMEOUT_MS,
        }
    }
}

#[derive(Clone)]
struct BackendInfo {
    pool: UpstreamPool,
    timing: RouteTiming,
}

#[derive(Clone)]
struct DirectorInfo {
    pool: UpstreamPool,
    timing: RouteTiming,
}

#[derive(Clone)]
struct InvocationPolicy {
    listeners: Vec<SocketAddr>,
    storage: Option<CacheStore>,
    default_ttl_ms: u64,
    default_grace_ms: u64,
    default_keep_ms: u64,
}

struct Lowerer<'a> {
    graph: &'a SourceGraph,
    source_invocation: &'a InvocationFacts,
    backends: &'a [Backend],
    directors: &'a [Director],
    modern_directors: &'a [ModernDirector],
    subroutines: &'a [Subroutine],
    statements: &'a [StatementDecision],
    imports: &'a [VmodImport],
    vmod_objects: &'a [VmodObject],
    diagnostics: Vec<Diagnostic>,
    blocker: Option<LoweringBlocker>,
    draft: CanonicalDraft,
    provenance: Vec<CanonicalProvenance<Provenance>>,
    backend_infos: BTreeMap<usize, BackendInfo>,
    director_infos: BTreeMap<usize, DirectorInfo>,
    invocation_policy: Option<InvocationPolicy>,
    root_origin: Option<Provenance>,
}

/// Lowers a complete semantic Varnish graph, retaining the semantic diagnostics accumulated by
/// parsing and resolution. No runtime code is invoked during this pass.
pub(super) fn lower(
    graph: &SourceGraph,
    invocation: &InvocationFacts,
    backends: &[Backend],
    directors: &[Director],
    modern_directors: &[ModernDirector],
    subroutines: &[Subroutine],
    statements: &[StatementDecision],
    imports: &[VmodImport],
    vmod_objects: &[VmodObject],
    diagnostics: Vec<Diagnostic>,
) -> (VarnishCanonicalCandidate, LoweringStatus, Vec<Diagnostic>) {
    Lowerer::new(
        graph,
        invocation,
        backends,
        directors,
        modern_directors,
        subroutines,
        statements,
        imports,
        vmod_objects,
        diagnostics,
    )
    .run()
}

impl<'a> Lowerer<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        graph: &'a SourceGraph,
        invocation: &'a InvocationFacts,
        backends: &'a [Backend],
        directors: &'a [Director],
        modern_directors: &'a [ModernDirector],
        subroutines: &'a [Subroutine],
        statements: &'a [StatementDecision],
        imports: &'a [VmodImport],
        vmod_objects: &'a [VmodObject],
        diagnostics: Vec<Diagnostic>,
    ) -> Self {
        Self {
            graph,
            source_invocation: invocation,
            backends,
            directors,
            modern_directors,
            subroutines,
            statements,
            imports,
            vmod_objects,
            diagnostics,
            blocker: None,
            draft: CanonicalDraft::default(),
            provenance: Vec::new(),
            backend_infos: BTreeMap::new(),
            director_infos: BTreeMap::new(),
            invocation_policy: None,
            root_origin: graph.root.and_then(|root| {
                graph.source(root).map(|source| Provenance {
                    span: source.source.full_span(),
                    include_stack: Vec::new(),
                })
            }),
        }
    }

    fn run(mut self) -> (VarnishCanonicalCandidate, LoweringStatus, Vec<Diagnostic>) {
        let has_canonical_graph = !self.subroutines.is_empty();
        self.scan_graph();
        if has_canonical_graph && !self.has_errors() {
            self.lower_invocation();
        }
        if has_canonical_graph && !self.has_errors() {
            self.lower_backends();
        }
        if has_canonical_graph && !self.has_errors() {
            self.lower_builtin_graph();
        }

        let config = has_canonical_graph.then(|| self.finalize()).flatten();
        let status = if config.is_some() {
            LoweringStatus::Lowered
        } else if !has_canonical_graph {
            LoweringStatus::Blocked(LoweringBlocker::NoCanonicalGraph)
        } else {
            LoweringStatus::Blocked(self.blocker.unwrap_or_else(|| {
                if self.has_source_errors() {
                    LoweringBlocker::InvalidSource
                } else {
                    LoweringBlocker::Validation
                }
            }))
        };
        let source_metadata = SourceImportMetadata {
            original_sources: self
                .graph
                .sources
                .iter()
                .map(|source| source.source.clone())
                .collect(),
            ..SourceImportMetadata::default()
        };
        let candidate = VarnishCanonicalCandidate {
            draft: self.draft,
            provenance: self.provenance,
            deployment_requirements: Vec::new(),
            activation_requirements: Vec::new(),
            operational_overlays: Vec::new(),
            source_metadata,
            config,
        };
        let (_, diagnostics) = crate::Report::new((), self.diagnostics).into_parts();
        (candidate, status, diagnostics)
    }

    fn scan_graph(&mut self) {
        if !self.graph.snapshot_stable {
            self.block(
                LoweringBlocker::InvalidSource,
                E_SOURCE_CHANGED,
                DiagnosticStage::Source,
                "VCL source snapshot is not stable",
                self.root_span(),
            );
        }
        for import in self.imports.to_vec() {
            self.block(
                LoweringBlocker::UnsupportedBehavior,
                E_VCL_LOWERING_BLOCKED,
                DiagnosticStage::Lower,
                format!(
                    "VMOD import `{}` is outside the exact Varnish lowering subset",
                    display(&import.module)
                ),
                Some(import.provenance.span),
            );
        }
        for object in self.vmod_objects.to_vec() {
            self.block(
                LoweringBlocker::UnsupportedBehavior,
                E_VCL_LOWERING_BLOCKED,
                DiagnosticStage::Lower,
                format!(
                    "VMOD object `{}` is outside the exact Varnish lowering subset",
                    display(&object.name)
                ),
                Some(object.provenance.span),
            );
        }
        for director in self.modern_directors.to_vec() {
            self.block(
                LoweringBlocker::UnsupportedBehavior,
                E_VCL_LOWERING_BLOCKED,
                DiagnosticStage::Lower,
                format!(
                    "modern director `{}` is outside the exact Varnish lowering subset",
                    display(&director.name)
                ),
                Some(director.provenance.span),
            );
        }
        for (subroutine_index, subroutine) in self.subroutines.iter().enumerate() {
            let kind = subroutine.kind;
            let name = subroutine.name.clone();
            let span = subroutine.provenance.span;
            let has_statements = self
                .statements
                .iter()
                .any(|statement| statement.subroutine == subroutine_index);
            if kind == SubroutineKind::Custom {
                self.block(
                    LoweringBlocker::UnsupportedSubroutine,
                    E_VCL_UNSUPPORTED_SUBROUTINE,
                    DiagnosticStage::Lower,
                    format!("custom VCL subroutine `{}` is not lowered", display(&name)),
                    Some(span),
                );
            } else if !matches!(
                kind,
                SubroutineKind::Recv
                    | SubroutineKind::Hash
                    | SubroutineKind::BackendFetch
                    | SubroutineKind::BackendResponse
                    | SubroutineKind::Deliver
            ) && has_statements
            {
                self.block(
                    LoweringBlocker::UnsupportedSubroutine,
                    E_VCL_UNSUPPORTED_SUBROUTINE,
                    DiagnosticStage::Lower,
                    format!(
                        "VCL phase `{}` is outside the exact lowering graph",
                        display(&name)
                    ),
                    Some(span),
                );
            }
        }
        for statement in self.statements.to_vec() {
            match &statement.classification {
                StatementClassification::Conditional(_) => self.block_statement(
                    LoweringBlocker::SemanticMismatch,
                    E_VCL_SEMANTIC_MISMATCH,
                    &statement,
                    "conditional VCL control flow is not representable by the canonical route/cache graph",
                ),
                StatementClassification::SubroutineCall { .. } => self.block_statement(
                    LoweringBlocker::UnsupportedSubroutine,
                    E_VCL_UNSUPPORTED_SUBROUTINE,
                    &statement,
                    "subroutine calls are not lowered",
                ),
                StatementClassification::Dynamic(_) | StatementClassification::Unsupported(_) => {
                    self.block_statement(
                        LoweringBlocker::UnsupportedBehavior,
                        E_VCL_LOWERING_BLOCKED,
                        &statement,
                        "dynamic or unsupported VCL behavior is not lowered",
                    );
                }
                StatementClassification::Invalid => self.block_statement(
                    LoweringBlocker::InvalidSource,
                    E_VCL_LOWERING_BLOCKED,
                    &statement,
                    "invalid VCL statement cannot be lowered",
                ),
                StatementClassification::Invalidation(_) => self.block_statement(
                    LoweringBlocker::SemanticMismatch,
                    E_VCL_SEMANTIC_MISMATCH,
                    &statement,
                    "VCL invalidation is not equivalent to the canonical HTTP cache interface",
                ),
                StatementClassification::Feature(FeatureBehavior::Esi { .. }) => self.block_statement(
                    LoweringBlocker::UnsupportedBehavior,
                    E_UNSUPPORTED_FEATURE,
                    &statement,
                    "ESI is outside the exact Varnish lowering subset",
                ),
                StatementClassification::Response(_) => {
                    self.block_statement(
                        LoweringBlocker::UnsupportedBehavior,
                        E_UNSUPPORTED_FEATURE,
                        &statement,
                        "VCL response generation is outside the exact HTTP route subset",
                    );
                }
                StatementClassification::NewDirector { .. }
                | StatementClassification::DirectorMethod { .. } => self.block_statement(
                    LoweringBlocker::UnsupportedBehavior,
                    E_VCL_LOWERING_BLOCKED,
                    &statement,
                    "runtime director construction is outside the exact lowering subset",
                ),
                _ => {}
            }
        }
    }

    fn lower_invocation(&mut self) {
        if self.source_invocation.truncated {
            self.block(
                LoweringBlocker::Invocation,
                E_SOURCE_LIMIT,
                DiagnosticStage::Source,
                "varnishd invocation was truncated before exact lowering",
                self.root_span(),
            );
        }
        if !self.source_invocation.unsupported_arguments.is_empty() {
            for argument in self.source_invocation.unsupported_arguments.clone() {
                self.block(
                    LoweringBlocker::Invocation,
                    E_VCL_LOWERING_BLOCKED,
                    DiagnosticStage::Lower,
                    format!("unsupported varnishd argument `{argument}` blocks lowering"),
                    self.root_span(),
                );
            }
        }

        let mut listeners = Vec::new();
        let mut default_ttl_ms = DEFAULT_TTL_MS;
        let mut default_grace_ms = DEFAULT_GRACE_MS;
        let mut default_keep_ms = DEFAULT_KEEP_MS;
        for startup in self.source_invocation.startup.clone() {
            match startup {
                super::StartupFact::Listen(value) => match parse_listen(&value) {
                    Some(address) => listeners.push(address),
                    None => self.block(
                        LoweringBlocker::Invocation,
                        E_VCL_SEMANTIC_MISMATCH,
                        DiagnosticStage::Lower,
                        format!("varnishd listen address `{value}` is not a canonical IP socket"),
                        self.root_span(),
                    ),
                },
                super::StartupFact::Parameter(setting) => {
                    let target = match setting.name.as_str() {
                        "default_ttl" => &mut default_ttl_ms,
                        "default_grace" => &mut default_grace_ms,
                        "default_keep" => &mut default_keep_ms,
                        _ => {
                            self.block(
                                LoweringBlocker::Invocation,
                                E_VCL_LOWERING_BLOCKED,
                                DiagnosticStage::Lower,
                                format!(
                                    "varnishd parameter `{}` has no exact canonical mapping",
                                    setting.name
                                ),
                                self.root_span(),
                            );
                            continue;
                        }
                    };
                    let Some(value) = setting.value.as_deref().and_then(parse_duration_ms_bytes)
                    else {
                        self.block(
                            LoweringBlocker::Invocation,
                            E_VCL_SEMANTIC_MISMATCH,
                            DiagnosticStage::Lower,
                            format!("varnishd parameter `{}` is not a finite millisecond duration", setting.name),
                            self.root_span(),
                        );
                        continue;
                    };
                    *target = value;
                }
                super::StartupFact::Foreground => {}
                super::StartupFact::Vcl(value) => {
                    if self
                        .graph
                        .root
                        .and_then(|root| self.graph.source(root))
                        .and_then(|source| source.canonical_path.as_deref())
                        .is_some_and(|path| path.to_string_lossy() != value)
                    {
                        self.block(
                            LoweringBlocker::Invocation,
                            E_VCL_SEMANTIC_MISMATCH,
                            DiagnosticStage::Lower,
                            "varnishd -f does not identify the imported VCL root",
                            self.root_span(),
                        );
                    }
                }
                super::StartupFact::Management(_)
                | super::StartupFact::Instance(_)
                | super::StartupFact::Jail(_)
                | super::StartupFact::Secret(_)
                | super::StartupFact::PidFile(_) => self.block(
                    LoweringBlocker::Invocation,
                    E_VCL_LOWERING_BLOCKED,
                    DiagnosticStage::Lower,
                    "varnishd deployment or management startup options are not part of the exact canonical runtime",
                    self.root_span(),
                ),
            }
        }
        listeners.sort_unstable();
        listeners.dedup();
        if listeners.is_empty() {
            listeners.push(DEFAULT_LISTEN);
        }
        if default_grace_ms > default_keep_ms {
            self.block(
                LoweringBlocker::SemanticMismatch,
                E_VCL_SEMANTIC_MISMATCH,
                DiagnosticStage::Lower,
                "Varnish default grace exceeds default keep and cannot be represented by the canonical cache timeline",
                self.root_span(),
            );
        }

        let storage = self.lower_storage();
        self.invocation_policy = Some(InvocationPolicy {
            listeners,
            storage,
            default_ttl_ms,
            default_grace_ms,
            default_keep_ms,
        });
    }

    fn lower_storage(&mut self) -> Option<CacheStore> {
        if self.source_invocation.storage.len() > 1 {
            self.block(
                LoweringBlocker::Invocation,
                E_VCL_SEMANTIC_MISMATCH,
                DiagnosticStage::Lower,
                "multiple varnishd storage backends do not have one canonical cache-store identity",
                self.root_span(),
            );
            return None;
        }
        let Some(storage) = self.source_invocation.storage.first().cloned() else {
            return Some(memory_store("varnish-memory", DEFAULT_MEMORY_BYTES));
        };
        let name = storage
            .name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "varnish-cache".into());
        match storage.kind {
            super::StorageKind::Malloc => {
                let Some(max_bytes) = storage
                    .arguments
                    .first()
                    .and_then(|value| parse_size(value))
                else {
                    self.block(
                        LoweringBlocker::Invocation,
                        E_VCL_SEMANTIC_MISMATCH,
                        DiagnosticStage::Lower,
                        "malloc storage requires one exact bounded size",
                        self.root_span(),
                    );
                    return None;
                };
                Some(memory_store(&name, max_bytes))
            }
            super::StorageKind::File => {
                let Some(path) = storage.arguments.first().and_then(canonical_storage_path) else {
                    self.block(
                        LoweringBlocker::Invocation,
                        E_VCL_SEMANTIC_MISMATCH,
                        DiagnosticStage::Lower,
                        "file storage requires one safe absolute canonical root",
                        self.root_span(),
                    );
                    return None;
                };
                let Some(max_bytes) = storage.arguments.get(1).and_then(|value| parse_size(value))
                else {
                    self.block(
                        LoweringBlocker::Invocation,
                        E_VCL_SEMANTIC_MISMATCH,
                        DiagnosticStage::Lower,
                        "file storage requires one exact bounded size",
                        self.root_span(),
                    );
                    return None;
                };
                Some(disk_store(&name, path, max_bytes))
            }
            super::StorageKind::None => None,
            super::StorageKind::Persistent
            | super::StorageKind::DeprecatedPersistent
            | super::StorageKind::Umem
            | super::StorageKind::Unknown(_) => {
                self.block(
                    LoweringBlocker::UnsupportedBehavior,
                    E_VCL_LOWERING_BLOCKED,
                    DiagnosticStage::Lower,
                    "selected varnishd storage backend is outside the exact memory/disk cache subset",
                    self.root_span(),
                );
                None
            }
        }
    }

    fn lower_backends(&mut self) {
        for (index, backend) in self.backends.to_vec().into_iter().enumerate() {
            if let Some(info) = self.lower_backend(&backend) {
                self.backend_infos.insert(index, info);
            }
        }
        for (index, director) in self.directors.to_vec().into_iter().enumerate() {
            if let Some(info) = self.lower_director(&director) {
                self.director_infos.insert(index, info);
            }
        }
        let pools = self
            .backend_infos
            .values()
            .map(|info| &info.pool)
            .chain(self.director_infos.values().map(|info| &info.pool))
            .cloned()
            .collect::<Vec<_>>();
        for (index, pool) in pools.iter().enumerate() {
            self.draft.upstream_pools.push(pool.clone());
            let path = format!("/upstream_pools/{index}");
            let origin = self.pool_origin(pool).into_iter().collect::<Vec<_>>();
            self.record(path.clone(), origin.clone());
            for suffix in ["/name", "/servers", "/algorithm", "/connection_reuse"] {
                self.record(format!("{path}{suffix}"), origin.clone());
            }
            for server_index in 0..pool.servers.len() {
                let server_path = format!("{path}/servers/{server_index}");
                self.record(server_path.clone(), origin.clone());
                self.record(format!("{server_path}/name"), origin.clone());
                self.record(format!("{server_path}/endpoint"), origin.clone());
            }
        }
    }

    fn lower_backend(&mut self, backend: &Backend) -> Option<BackendInfo> {
        let name = match String::from_utf8(backend.name.clone()) {
            Ok(name) if !name.is_empty() => name,
            _ => {
                self.block_backend(backend, "backend identity is not valid UTF-8");
                return None;
            }
        };
        if matches!(&backend.kind, BackendKind::None) {
            return None;
        }
        if matches!(&backend.kind, BackendKind::Dynamic) {
            self.block_backend(backend, "dynamic backend declaration is not lowered");
            return None;
        }
        let mut host = None;
        let mut port = None;
        let mut path = None;
        let mut timing = RouteTiming::default();
        let mut max_connections = None;
        for property in &backend.properties {
            match property {
                BackendProperty::Host(value) => {
                    if host.replace(value).is_some() {
                        self.block_backend(backend, "backend host is assigned more than once");
                    }
                }
                BackendProperty::Port(value) => {
                    if port.replace(value).is_some() {
                        self.block_backend(backend, "backend port is assigned more than once");
                    }
                }
                BackendProperty::Path(value) => {
                    if path.replace(value).is_some() {
                        self.block_backend(backend, "backend Unix path is assigned more than once");
                    }
                }
                BackendProperty::ConnectTimeout(value) => {
                    if let Some(milliseconds) = duration_ms(value) {
                        timing.connect_ms = milliseconds;
                    } else {
                        self.block_backend(
                            backend,
                            "backend connect timeout is not a finite millisecond duration",
                        );
                    }
                }
                BackendProperty::FirstByteTimeout(value) => {
                    if let Some(milliseconds) = duration_ms(value) {
                        timing.read_ms = milliseconds;
                    } else {
                        self.block_backend(
                            backend,
                            "backend first-byte timeout is not a finite millisecond duration",
                        );
                    }
                }
                BackendProperty::BetweenBytesTimeout(value) => {
                    if let Some(milliseconds) = duration_ms(value) {
                        timing.write_ms = milliseconds;
                    } else {
                        self.block_backend(
                            backend,
                            "backend between-bytes timeout is not a finite millisecond duration",
                        );
                    }
                }
                BackendProperty::MaxConnections(value) => {
                    if let Some(value) = integer(value).filter(|value| *value > 0) {
                        max_connections = Some(value);
                    } else {
                        self.block_backend(
                            backend,
                            "backend max_connections is not a positive integer",
                        );
                    }
                }
                BackendProperty::Probe(_)
                | BackendProperty::ProxyHeader(_)
                | BackendProperty::Unsupported { .. } => {
                    self.block_backend(backend, "backend property has no exact canonical mapping");
                }
            }
        }
        let endpoint = match &backend.kind {
            BackendKind::Network {
                host: kind_host,
                port: kind_port,
            } => {
                let host = host.or(kind_host.as_ref());
                let port = port.or(kind_port.as_ref());
                let Some(host) = host.and_then(static_text) else {
                    self.block_backend(backend, "network backend host is not static");
                    return None;
                };
                let port = port.and_then(static_port).unwrap_or(80);
                if let Some(address) = host.parse::<IpAddr>().ok() {
                    UpstreamEndpoint::Socket {
                        address: SocketAddr::new(address, port),
                    }
                } else if is_dns_name(&host) {
                    UpstreamEndpoint::Dns { host, port }
                } else {
                    self.block_backend(
                        backend,
                        "network backend host is not a canonical IP or DNS name",
                    );
                    return None;
                }
            }
            BackendKind::Unix { path: kind_path } => {
                let path = path.or(Some(kind_path));
                let Some(path) = path.and_then(static_text).and_then(canonical_unix_path) else {
                    self.block_backend(
                        backend,
                        "Unix backend path is not a safe absolute socket path",
                    );
                    return None;
                };
                UpstreamEndpoint::Unix { path }
            }
            BackendKind::None | BackendKind::Dynamic => return None,
        };
        let server = UpstreamServer {
            name: name.clone(),
            endpoint,
            max_connections,
            dns_resolution: DnsResolutionPolicy::OnConnect,
        };
        Some(BackendInfo {
            pool: UpstreamPool {
                name,
                servers: vec![server],
                endpoints: Vec::new(),
                algorithm: UpstreamAlgorithm::RoundRobin,
                health_check: None,
                tls: None,
                http_versions: Default::default(),
                queue_timeout_ms: None,
                connect_timeout_ms: None,
                server_timeout_ms: None,
                connection_reuse: UpstreamConnectionReuse::Safe,
            },
            timing,
        })
    }

    fn lower_director(&mut self, director: &Director) -> Option<DirectorInfo> {
        let name = match String::from_utf8(director.name.clone()) {
            Ok(name) if !name.is_empty() => name,
            _ => {
                self.block_director(director, "director identity is not valid UTF-8");
                return None;
            }
        };
        let algorithm = match &director.policy {
            DirectorKind::RoundRobin => UpstreamAlgorithm::RoundRobin,
            DirectorKind::Fallback => UpstreamAlgorithm::First,
            DirectorKind::Random | DirectorKind::Hash | DirectorKind::Unknown(_) => {
                self.block_director(director, "director policy has no exact canonical algorithm");
                return None;
            }
        };
        if !director.unsupported_properties.is_empty() || director.members.is_empty() {
            self.block_director(director, "director properties or membership are not exact");
            return None;
        }
        let mut servers = Vec::new();
        let mut timing = None;
        for member in &director.members {
            let BackendReference::Backend { declaration, .. } = member else {
                self.block_director(
                    director,
                    "director member is not a static backend reference",
                );
                return None;
            };
            let Some(info) = self.backend_infos.get(declaration).cloned() else {
                self.block_director(director, "director member backend could not be lowered");
                return None;
            };
            if servers
                .iter()
                .any(|server: &UpstreamServer| server.endpoint == info.pool.servers[0].endpoint)
            {
                self.block_director(director, "director repeats one backend endpoint");
                return None;
            }
            if timing.is_some_and(|candidate| !same_timing(candidate, info.timing)) {
                self.block_director(director, "director members use different timeout semantics");
                return None;
            }
            timing = Some(info.timing);
            servers.push(info.pool.servers[0].clone());
        }
        Some(DirectorInfo {
            pool: UpstreamPool {
                name,
                servers,
                endpoints: Vec::new(),
                algorithm,
                health_check: None,
                tls: None,
                http_versions: Default::default(),
                queue_timeout_ms: None,
                connect_timeout_ms: None,
                server_timeout_ms: None,
                connection_reuse: UpstreamConnectionReuse::Safe,
            },
            timing: timing.unwrap_or_default(),
        })
    }

    fn lower_builtin_graph(&mut self) {
        let recv = self.phase(SubroutineKind::Recv);
        let mut selected = None;
        let mut cache_enabled = true;
        let mut request_headers = Vec::new();
        let mut terminal = None;
        for (index, statement) in recv.iter().enumerate() {
            match &statement.classification {
                StatementClassification::BackendSelection(reference) => {
                    if selected.replace(reference.clone()).is_some() {
                        self.block_statement(
                            LoweringBlocker::SemanticMismatch,
                            E_VCL_SEMANTIC_MISMATCH,
                            &statement,
                            "backend selection is assigned more than once",
                        );
                    }
                }
                StatementClassification::HeaderMutation(mutation)
                    if matches!(
                        mutation.scope,
                        HeaderScope::Request | HeaderScope::BackendRequest
                    ) =>
                {
                    if let Some(mutation) = request_mutation(mutation) {
                        request_headers.push(mutation);
                    } else {
                        self.block_statement(
                            LoweringBlocker::SemanticMismatch,
                            E_VCL_SEMANTIC_MISMATCH,
                            &statement,
                            "request header mutation is not a static canonical set/remove operation",
                        );
                    }
                }
                StatementClassification::CacheDecision(action) => {
                    if terminal.is_some() || index + 1 != recv.len() {
                        self.block_statement(
                            LoweringBlocker::SemanticMismatch,
                            E_VCL_SEMANTIC_MISMATCH,
                            &statement,
                            "terminal VCL flow action is not the final statement in its phase",
                        );
                    }
                    terminal = Some(*action);
                    match action {
                        FlowAction::Hash | FlowAction::Lookup => cache_enabled = true,
                        FlowAction::Pass => cache_enabled = false,
                        FlowAction::Pipe => self.block_statement(
                            LoweringBlocker::UnsupportedBehavior,
                            E_UNSUPPORTED_FEATURE,
                            &statement,
                            "pipe mode has no equivalent canonical HTTP service action",
                        ),
                        _ => self.block_statement(
                            LoweringBlocker::SemanticMismatch,
                            E_VCL_SEMANTIC_MISMATCH,
                            &statement,
                            "VCL flow action is not representable by the canonical HTTP service",
                        ),
                    }
                }
                _ => self.block_statement(
                    LoweringBlocker::UnsupportedBehavior,
                    E_VCL_LOWERING_BLOCKED,
                    &statement,
                    "VCL receive-phase statement is outside the exact linear graph",
                ),
            }
        }
        if terminal.is_none() {
            cache_enabled = true;
        }

        self.validate_hash_phase();
        let mut response_headers = Vec::new();
        let mut cache_ttl = None;
        let mut cache_grace = None;
        let mut cache_keep = None;
        let mut explicit_ttl = false;
        for statement in self
            .phase(SubroutineKind::BackendResponse)
            .into_iter()
            .chain(self.phase(SubroutineKind::Deliver))
        {
            match &statement.classification {
                StatementClassification::CacheLifetime(lifetime) => {
                    let value = duration_ms(&lifetime.value);
                    if lifetime.operator != super::AssignmentOperator::Set || value.is_none() {
                        self.block_statement(
                            LoweringBlocker::SemanticMismatch,
                            E_VCL_SEMANTIC_MISMATCH,
                            &statement,
                            "cache lifetime must be assigned one finite whole-millisecond literal",
                        );
                        continue;
                    }
                    let value = value.expect("checked cache lifetime");
                    let slot = match lifetime.field {
                        CacheLifetimeField::Ttl => {
                            explicit_ttl = true;
                            &mut cache_ttl
                        }
                        CacheLifetimeField::Grace => &mut cache_grace,
                        CacheLifetimeField::Keep => &mut cache_keep,
                    };
                    if slot.replace(value).is_some() {
                        self.block_statement(
                            LoweringBlocker::SemanticMismatch,
                            E_VCL_SEMANTIC_MISMATCH,
                            &statement,
                            "cache lifetime field is assigned more than once",
                        );
                    }
                }
                StatementClassification::CacheFlag(flag) => match flag {
                    CacheFlag::Uncacheable(value) => match boolean(value) {
                        Some(false) => {}
                        Some(true) => self.block_statement(
                            LoweringBlocker::SemanticMismatch,
                            E_VCL_SEMANTIC_MISMATCH,
                            &statement,
                            "uncacheable backend responses do not have an exact canonical response policy",
                        ),
                        None => self.block_statement(
                            LoweringBlocker::SemanticMismatch,
                            E_VCL_SEMANTIC_MISMATCH,
                            &statement,
                            "uncacheable must be assigned a static boolean",
                        ),
                    },
                    CacheFlag::HitForPass { .. } | CacheFlag::BackgroundFetch(_) => self
                        .block_statement(
                            LoweringBlocker::UnsupportedBehavior,
                            E_UNSUPPORTED_FEATURE,
                            &statement,
                            "hit-for-pass or background fetch is outside the exact cache subset",
                        ),
                },
                StatementClassification::Feature(FeatureBehavior::Compression { operation, enabled }) => {
                    match (operation, boolean(enabled)) {
                        (CompressionOperation::Gzip | CompressionOperation::Gunzip, Some(false)) => {}
                        (CompressionOperation::Gzip, Some(true)) => self.block_statement(
                            LoweringBlocker::SemanticMismatch,
                            E_VCL_SEMANTIC_MISMATCH,
                            &statement,
                            "Varnish gzip defaults do not exactly identify the canonical gzip policy",
                        ),
                        (CompressionOperation::Gunzip, Some(true)) => self.block_statement(
                            LoweringBlocker::UnsupportedBehavior,
                            E_UNSUPPORTED_FEATURE,
                            &statement,
                            "backend gunzip has no canonical response equivalent",
                        ),
                        (_, None) => self.block_statement(
                            LoweringBlocker::SemanticMismatch,
                            E_VCL_SEMANTIC_MISMATCH,
                            &statement,
                            "compression behavior must be assigned a static boolean",
                        ),
                    }
                }
                StatementClassification::HeaderMutation(mutation)
                    if matches!(
                        mutation.scope,
                        HeaderScope::BackendResponse | HeaderScope::Response | HeaderScope::Object
                    ) =>
                {
                    if let Some(mutation) = response_mutation(mutation) {
                        response_headers.push(mutation);
                    } else {
                        self.block_statement(
                            LoweringBlocker::SemanticMismatch,
                            E_VCL_SEMANTIC_MISMATCH,
                            &statement,
                            "response header mutation is not a static canonical set/add/remove operation",
                        );
                    }
                }
                StatementClassification::CacheDecision(FlowAction::Deliver) => {}
                _ => self.block_statement(
                    LoweringBlocker::UnsupportedBehavior,
                    E_VCL_LOWERING_BLOCKED,
                    &statement,
                    "VCL backend-response or deliver behavior is outside the exact cache graph",
                ),
            }
        }
        let backend_fetch_headers = self
            .phase(SubroutineKind::BackendFetch)
            .into_iter()
            .filter_map(|statement| match &statement.classification {
                StatementClassification::HeaderMutation(mutation)
                    if matches!(mutation.scope, HeaderScope::BackendRequest | HeaderScope::Request) =>
                {
                    request_mutation(mutation).or_else(|| {
                        self.block_statement(
                            LoweringBlocker::SemanticMismatch,
                            E_VCL_SEMANTIC_MISMATCH,
                            &statement,
                            "backend request header mutation is not a static canonical set/remove operation",
                        );
                        None
                    })
                }
                _ => {
                    self.block_statement(
                        LoweringBlocker::UnsupportedBehavior,
                        E_VCL_LOWERING_BLOCKED,
                        &statement,
                        "VCL backend-fetch behavior is outside the exact request graph",
                    );
                    None
                }
            })
            .collect::<Vec<_>>();
        request_headers.extend(backend_fetch_headers);

        if cache_enabled && !request_headers.is_empty() {
            self.block(
                LoweringBlocker::SemanticMismatch,
                E_VCL_SEMANTIC_MISMATCH,
                DiagnosticStage::Lower,
                "static request header mutation with cache changes backend-only semantics that the active cache plan rejects",
                self.root_span(),
            );
        }

        let selected = selected
            .as_ref()
            .and_then(|reference| self.selected_pool(reference));
        let Some((pool_name, timing, backend_origin)) = selected else {
            self.block(
                LoweringBlocker::SemanticMismatch,
                E_VCL_SEMANTIC_MISMATCH,
                DiagnosticStage::Lower,
                "VCL graph does not select one static backend or director",
                self.root_span(),
            );
            return;
        };
        let Some(invocation_policy) = self.invocation_policy.clone() else {
            return;
        };
        let cache = if cache_enabled {
            let Some(store) = invocation_policy.storage.as_ref() else {
                self.block(
                    LoweringBlocker::SemanticMismatch,
                    E_VCL_SEMANTIC_MISMATCH,
                    DiagnosticStage::Lower,
                    "VCL cache flow requires a memory or disk varnishd storage backend",
                    self.root_span(),
                );
                return;
            };
            let store_name = cache_store_name(store).to_owned();
            let ttl = cache_ttl.unwrap_or(invocation_policy.default_ttl_ms);
            let grace = cache_grace.unwrap_or(invocation_policy.default_grace_ms);
            let keep = cache_keep.unwrap_or(invocation_policy.default_keep_ms);
            if grace > keep {
                self.block(
                    LoweringBlocker::SemanticMismatch,
                    E_VCL_SEMANTIC_MISMATCH,
                    DiagnosticStage::Lower,
                    "VCL grace exceeds keep and cannot be represented by the canonical cache timeline",
                    self.root_span(),
                );
            }
            Some(Box::new(HttpCachePolicy {
                store: store_name,
                methods: vec!["GET".into(), "HEAD".into()],
                key_components: vec![
                    CacheKeyComponent::Scheme,
                    CacheKeyComponent::NormalizedHost,
                    CacheKeyComponent::PathAndQuery,
                ],
                use_origin_cache_control: !explicit_ttl,
                default_ttl_ms: ttl,
                status_ttls: Vec::new(),
                grace_ms: grace,
                keep_ms: keep,
                revalidate: true,
                collapsed_forwarding: true,
                stale_on: Vec::new(),
                bypass_request: Vec::new(),
                no_store_request: Vec::new(),
                no_store_response: Vec::new(),
                set_cookie_policy: Default::default(),
                authorization_policy: Default::default(),
                vary_policy: Default::default(),
                surrogate_tags: None,
                purge_authorization: None,
            }))
        } else {
            None
        };

        let pool_origin =
            backend_origin.unwrap_or_else(|| self.root_origin.clone().expect("root origin"));
        let service_name = "varnish-http".to_owned();
        let route = HttpRoute {
            host: None,
            path: oxiroute_config::HttpPathSelector::SegmentPrefix { value: "/".into() },
            methods: Vec::new(),
            access_policy: None,
            policy: HttpRoutePolicy {
                max_request_body_bytes: None,
                connect_timeout_ms: timing.connect_ms,
                read_timeout_ms: timing.read_ms,
                write_timeout_ms: timing.write_ms,
                request_buffering: false,
                response_buffering: false,
            },
            action: HttpRouteAction::Proxy {
                upstream_pool: pool_name,
                policy: HttpProxyPolicy {
                    upstream_host: HttpUpstreamHost::PreserveIncoming,
                    upstream_path_rewrite: None,
                    request_headers,
                    response_headers,
                    response_cookie_path_rewrites: Vec::new(),
                    response_cookie_attributes: Vec::new(),
                    retry: Default::default(),
                    cache,
                },
            },
        };
        self.draft.http_services.push(HttpService {
            name: service_name.clone(),
            routes: vec![route],
            automatic_response_headers: false,
            upstream_io_timeout_ms: timing.read_ms,
            max_request_body_bytes: None,
            gzip: None,
            access_log: None,
        });
        self.record("/http_services/0".into(), vec![pool_origin.clone()]);
        for suffix in [
            "/name",
            "/automatic_response_headers",
            "/upstream_io_timeout_ms",
            "/max_request_body_bytes",
            "/routes/0",
            "/routes/0/path",
            "/routes/0/action",
            "/routes/0/action/upstream_pool",
            "/routes/0/action/policy",
        ] {
            self.record(
                format!("/http_services/0{suffix}"),
                vec![pool_origin.clone()],
            );
        }
        if cache_enabled {
            self.record(
                "/http_services/0/routes/0/action/policy/cache".into(),
                self.phase_origin(SubroutineKind::BackendResponse),
            );
        }
        for (index, address) in invocation_policy.listeners.iter().copied().enumerate() {
            self.draft.listeners.push(Listener {
                name: format!("varnish-listener-{}", index + 1),
                bind: ListenerBind::Socket { address },
                protocol: Protocol::Http,
                service: Some(service_name.clone()),
                tls_profile: None,
                max_connections: None,
                downstream_timeouts: DownstreamTimeoutPolicy::default(),
            });
            let path = format!("/listeners/{index}");
            self.record(path.clone(), vec![pool_origin.clone()]);
            self.record(format!("{path}/bind"), vec![pool_origin.clone()]);
            self.record(format!("{path}/service"), vec![pool_origin.clone()]);
            self.record(format!("{path}/protocol"), vec![pool_origin.clone()]);
        }
        if let Some(store) = invocation_policy.storage.as_ref() {
            let index = self.draft.cache_stores.len();
            self.draft.cache_stores.push(store.clone());
            let path = format!("/cache_stores/{index}");
            self.record(path.clone(), self.root_origin.clone().into_iter().collect());
            self.record(
                format!("{path}/name"),
                self.root_origin.clone().into_iter().collect(),
            );
        }
    }

    fn validate_hash_phase(&mut self) {
        let statements = self.phase(SubroutineKind::Hash);
        if statements.is_empty() {
            return;
        }
        let mut keys = Vec::new();
        let mut lookup = false;
        for statement in &statements {
            match &statement.classification {
                StatementClassification::Hash(expression) => keys.push(expression),
                StatementClassification::CacheDecision(FlowAction::Lookup) => lookup = true,
                _ => self.block_statement(
                    LoweringBlocker::SemanticMismatch,
                    E_VCL_SEMANTIC_MISMATCH,
                    &statement,
                    "VCL hash phase is not the canonical URL/Host hash graph",
                ),
            }
        }
        let exact = keys.len() == 2
            && keys
                .iter()
                .any(|expression| expression_name(expression) == Some(b"req.url"))
            && keys
                .iter()
                .any(|expression| expression_name(expression).is_some_and(is_host_name));
        if !exact || !lookup {
            let span = statements
                .first()
                .map(|statement| statement.provenance.span);
            self.block(
                LoweringBlocker::SemanticMismatch,
                E_VCL_SEMANTIC_MISMATCH,
                DiagnosticStage::Lower,
                "VCL hash phase must contain exactly req.url, req.http.Host, and return(lookup)",
                span,
            );
        }
    }

    fn selected_pool(
        &self,
        reference: &BackendReference,
    ) -> Option<(String, RouteTiming, Option<Provenance>)> {
        match reference {
            BackendReference::Backend { declaration, .. } => {
                self.backend_infos.get(declaration).map(|info| {
                    (
                        info.pool.name.clone(),
                        info.timing,
                        self.backend_origin(*declaration),
                    )
                })
            }
            BackendReference::Director {
                declaration,
                modern: false,
                ..
            } => self.director_infos.get(declaration).map(|info| {
                (
                    info.pool.name.clone(),
                    info.timing,
                    self.director_origin(*declaration),
                )
            }),
            BackendReference::Director { modern: true, .. }
            | BackendReference::None
            | BackendReference::Unresolved { .. }
            | BackendReference::Dynamic(_) => None,
        }
    }

    fn backend_origin(&self, declaration: usize) -> Option<Provenance> {
        self.backends
            .get(declaration)
            .map(|backend| backend.provenance.clone())
    }

    fn director_origin(&self, declaration: usize) -> Option<Provenance> {
        self.directors
            .get(declaration)
            .map(|director| director.provenance.clone())
    }

    fn pool_origin(&self, pool: &UpstreamPool) -> Option<Provenance> {
        self.backends
            .iter()
            .find(|backend| {
                String::from_utf8(backend.name.clone()).ok().as_deref() == Some(pool.name.as_str())
            })
            .map(|backend| backend.provenance.clone())
            .or_else(|| {
                self.directors
                    .iter()
                    .find(|director| {
                        String::from_utf8(director.name.clone()).ok().as_deref()
                            == Some(pool.name.as_str())
                    })
                    .map(|director| director.provenance.clone())
            })
            .or_else(|| self.root_origin.clone())
    }

    fn phase(&self, kind: SubroutineKind) -> Vec<StatementDecision> {
        let indices = self
            .subroutines
            .iter()
            .enumerate()
            .filter(|(_, subroutine)| subroutine.kind == kind)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        self.statements
            .iter()
            .filter(|statement| indices.contains(&statement.subroutine))
            .cloned()
            .collect()
    }

    fn phase_origin(&self, kind: SubroutineKind) -> Vec<Provenance> {
        self.phase(kind)
            .iter()
            .map(|statement| statement.provenance.clone())
            .collect()
    }

    fn finalize(&mut self) -> Option<Config> {
        if self.has_errors() {
            return None;
        }
        let mut config = self.draft.to_config();
        if let Err(error) = validate_config(&mut config) {
            self.block(
                LoweringBlocker::Validation,
                E_INVALID_VALUE,
                DiagnosticStage::Validate,
                format!("lowered Varnish canonical draft is invalid: {error}"),
                self.root_span(),
            );
            return None;
        }
        Some(config)
    }

    fn record(&mut self, path: String, origins: Vec<Provenance>) {
        if origins.is_empty() {
            return;
        }
        self.provenance.push(CanonicalProvenance { path, origins });
    }

    fn block_backend(&mut self, backend: &Backend, message: &'static str) {
        self.block(
            LoweringBlocker::SemanticMismatch,
            E_VCL_SEMANTIC_MISMATCH,
            DiagnosticStage::Lower,
            message,
            Some(backend.provenance.span),
        );
    }

    fn block_director(&mut self, director: &Director, message: &'static str) {
        self.block(
            LoweringBlocker::SemanticMismatch,
            E_VCL_SEMANTIC_MISMATCH,
            DiagnosticStage::Lower,
            message,
            Some(director.provenance.span),
        );
    }

    fn block_statement(
        &mut self,
        blocker: LoweringBlocker,
        code: crate::DiagnosticCode,
        statement: &StatementDecision,
        message: &'static str,
    ) {
        self.block(
            blocker,
            code,
            DiagnosticStage::Lower,
            message,
            Some(statement.provenance.span),
        );
    }

    fn block(
        &mut self,
        blocker: LoweringBlocker,
        code: crate::DiagnosticCode,
        stage: DiagnosticStage,
        message: impl Into<String>,
        span: Option<crate::Span>,
    ) {
        self.blocker.get_or_insert(blocker);
        let diagnostic = Diagnostic::new(code, Severity::Error, stage, message).with_include_stack(
            span.and_then(|span| self.provenance_for_span(span))
                .map(|provenance| provenance.include_stack)
                .unwrap_or_default(),
        );
        self.diagnostics.push(if let Some(span) = span {
            diagnostic.with_primary_span(span)
        } else {
            diagnostic
        });
    }

    fn provenance_for_span(&self, span: crate::Span) -> Option<Provenance> {
        self.statements
            .iter()
            .find(|statement| statement.provenance.span == span)
            .map(|statement| statement.provenance.clone())
            .or_else(|| self.root_origin.clone())
    }

    fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity() == Severity::Error)
    }

    fn has_source_errors(&self) -> bool {
        self.diagnostics.iter().any(|diagnostic| {
            diagnostic.severity() == Severity::Error
                && matches!(diagnostic.code(), E_SOURCE_CHANGED | E_SOURCE_LIMIT)
        })
    }

    fn root_span(&self) -> Option<crate::Span> {
        self.root_origin.as_ref().map(|origin| origin.span)
    }
}

fn memory_store(name: &str, max_bytes: u64) -> CacheStore {
    CacheStore::Memory {
        name: name.into(),
        max_bytes,
        max_entries: DEFAULT_MEMORY_ENTRIES,
        max_object_bytes: DEFAULT_OBJECT_BYTES.min(max_bytes),
        max_header_bytes: DEFAULT_HEADER_BYTES.min(max_bytes),
        max_key_bytes: DEFAULT_KEY_BYTES,
        max_tag_bytes: DEFAULT_TAG_BYTES,
        max_tags_per_object: DEFAULT_TAGS_PER_OBJECT,
        max_in_flight_fills: DEFAULT_IN_FLIGHT_FILLS,
        max_followers_per_fill: DEFAULT_FOLLOWERS_PER_FILL,
    }
}

fn disk_store(name: &str, root_directory: PathBuf, max_bytes: u64) -> CacheStore {
    CacheStore::Disk {
        name: name.into(),
        root_directory,
        max_bytes,
        max_files: DEFAULT_DISK_FILES,
        max_object_bytes: DEFAULT_OBJECT_BYTES.min(max_bytes),
        max_header_bytes: DEFAULT_HEADER_BYTES.min(max_bytes),
        max_key_bytes: DEFAULT_KEY_BYTES,
        max_tag_bytes: DEFAULT_TAG_BYTES,
        max_tags_per_object: DEFAULT_TAGS_PER_OBJECT,
        max_in_flight_fills: DEFAULT_IN_FLIGHT_FILLS,
        max_followers_per_fill: DEFAULT_FOLLOWERS_PER_FILL,
    }
}

fn cache_store_name(store: &CacheStore) -> &str {
    match store {
        CacheStore::Memory { name, .. } | CacheStore::Disk { name, .. } => name,
    }
}

fn parse_listen(value: &str) -> Option<SocketAddr> {
    if value.contains(',') {
        return None;
    }
    if let Some(port) = value.strip_prefix(':') {
        return port
            .parse()
            .ok()
            .map(|port| SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), port));
    }
    value.parse().ok()
}

fn canonical_storage_path(value: &String) -> Option<PathBuf> {
    let path = std::path::Path::new(value);
    let text = value.as_bytes();
    (path.is_absolute()
        && !text.contains(&0)
        && !value.contains("//")
        && !value.ends_with('/')
        && !value
            .split('/')
            .any(|segment| matches!(segment, "." | "..")))
    .then(|| path.to_path_buf())
}

fn canonical_unix_path(value: String) -> Option<PathBuf> {
    let mut output = String::new();
    for segment in value.split('/').filter(|segment| !segment.is_empty()) {
        if matches!(segment, "." | "..") {
            return None;
        }
        output.push('/');
        output.push_str(segment);
    }
    (!output.is_empty() && output.len() <= 107).then(|| output.into())
}

fn is_dns_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && !value.ends_with('.')
        && value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
}

fn same_timing(left: RouteTiming, right: RouteTiming) -> bool {
    left.connect_ms == right.connect_ms
        && left.read_ms == right.read_ms
        && left.write_ms == right.write_ms
}

fn expression_name(expression: &Expression) -> Option<&[u8]> {
    match &expression.kind {
        ExpressionKind::Name(value) => Some(&value.bytes),
        _ => None,
    }
}

fn is_host_name(value: &[u8]) -> bool {
    value.eq_ignore_ascii_case(b"req.http.host")
}

fn static_text(expression: &Expression) -> Option<String> {
    match &expression.kind {
        ExpressionKind::Literal(super::Literal::String(value))
        | ExpressionKind::Literal(super::Literal::Number(value)) => {
            String::from_utf8(value.bytes.clone()).ok()
        }
        _ => None,
    }
}

fn static_port(expression: &Expression) -> Option<u16> {
    static_text(expression)?
        .parse()
        .ok()
        .filter(|port| *port > 0)
}

fn integer(expression: &Expression) -> Option<u64> {
    static_text(expression)?.parse().ok()
}

fn boolean(expression: &Expression) -> Option<bool> {
    match expression_name(expression) {
        Some(b"true") => Some(true),
        Some(b"false") => Some(false),
        _ => None,
    }
}

fn duration_ms(expression: &Expression) -> Option<u64> {
    static_text(expression).and_then(|value| parse_duration_ms_bytes(&value))
}

fn parse_duration_ms_bytes(value: &str) -> Option<u64> {
    let split = value.as_bytes().iter().position(u8::is_ascii_alphabetic)?;
    let number = &value[..split];
    let unit = value[split..].to_ascii_lowercase();
    let unit_nanos = match unit.as_str() {
        "ns" => 1_u128,
        "us" => 1_000,
        "ms" => 1_000_000,
        "s" => 1_000_000_000,
        "m" => 60 * 1_000_000_000,
        "h" => 60 * 60 * 1_000_000_000,
        "d" => 24 * 60 * 60 * 1_000_000_000,
        "w" => 7 * 24 * 60 * 60 * 1_000_000_000,
        _ => return None,
    };
    let (whole, fraction) = number
        .split_once('.')
        .map_or((number, ""), |(whole, fraction)| (whole, fraction));
    if whole.is_empty()
        || fraction.len() > 9
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let scale = 10_u128.checked_pow(u32::try_from(fraction.len()).ok()?)?;
    let fraction_value = if fraction.is_empty() {
        0
    } else {
        fraction.parse().ok()?
    };
    let numerator = whole
        .parse::<u128>()
        .ok()?
        .checked_mul(scale)?
        .checked_add(fraction_value)?;
    let nanos = numerator.checked_mul(unit_nanos)?;
    if nanos % scale != 0 || (nanos / scale) % 1_000_000 != 0 {
        return None;
    }
    u64::try_from(nanos / scale / 1_000_000).ok()
}

fn parse_size(value: &str) -> Option<u64> {
    let split = value
        .as_bytes()
        .iter()
        .position(u8::is_ascii_alphabetic)
        .unwrap_or(value.len());
    let number = value[..split].parse::<u64>().ok()?;
    let multiplier = match value[split..].to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" => 1 << 10,
        "m" | "mb" => 1 << 20,
        "g" | "gb" => 1 << 30,
        "t" | "tb" => 1 << 40,
        "p" | "pb" => 1 << 50,
        _ => return None,
    };
    number.checked_mul(multiplier)
}

fn request_mutation(mutation: &HeaderMutation) -> Option<HttpRequestHeaderMutation> {
    let name = header_name(&mutation.name)?;
    match mutation.operation {
        HeaderOperation::Set => Some(HttpRequestHeaderMutation::Set {
            name,
            value: HttpRequestHeaderValue::Literal {
                value: header_value(mutation.value.as_ref()?)?,
            },
        }),
        HeaderOperation::Remove => Some(HttpRequestHeaderMutation::Remove { name }),
        HeaderOperation::Append => None,
    }
}

fn response_mutation(mutation: &HeaderMutation) -> Option<HttpResponseHeaderMutation> {
    let name = header_name(&mutation.name)?;
    match mutation.operation {
        HeaderOperation::Set => Some(HttpResponseHeaderMutation::Set {
            name,
            value: header_value(mutation.value.as_ref()?)?,
            always: true,
        }),
        HeaderOperation::Append => Some(HttpResponseHeaderMutation::Add {
            name,
            value: header_value(mutation.value.as_ref()?)?,
            always: true,
        }),
        HeaderOperation::Remove => Some(HttpResponseHeaderMutation::Remove { name }),
    }
}

fn header_name(value: &[u8]) -> Option<String> {
    let name = HeaderName::from_bytes(value).ok()?;
    Some(name.as_str().to_owned())
}

fn header_value(expression: &Expression) -> Option<String> {
    let value = static_text(expression)?;
    HeaderValue::from_str(&value).ok()?;
    Some(value)
}

fn display(value: &[u8]) -> String {
    String::from_utf8(value.to_vec()).unwrap_or_else(|_| "<non-UTF-8>".into())
}
