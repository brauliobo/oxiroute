use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::DecimalCounter;
use crate::{
    TOPOLOGY_SCHEMA_VERSION, TopologyEdge, TopologyEdgeKind, TopologyNode, TopologyNodeKind,
    TopologySnapshot, monitoring::RuntimeHealthSnapshot,
};

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TopologyResponse {
    schema_version: u32,
    state: TopologyStateDto,
    nodes: Vec<TopologyNodeDto>,
    edges: Vec<TopologyEdgeDto>,
    overlays: Vec<TopologyOverlayDto>,
}

impl TopologyResponse {
    pub(crate) fn active(
        topology: &TopologySnapshot,
        runtime: &RuntimeHealthSnapshot,
    ) -> Result<Self, String> {
        let response = topology
            .response_value(runtime)
            .map_err(|error| error.to_string())?;
        serde_json::from_value(response)
            .map_err(|error| format!("could not project active topology API DTO: {error}"))
    }

    pub(crate) fn candidate(
        topology: &TopologySnapshot,
        sampled_at_unix_ms: u64,
    ) -> Result<Self, String> {
        Self::project(
            topology,
            TopologyStateDto {
                config: ConfigStateDto::Candidate,
                runtime: RuntimeStateDto::NotActive,
                sampled_at_unix_ms,
            },
            Vec::new(),
        )
    }

    fn project(
        topology: &TopologySnapshot,
        state: TopologyStateDto,
        overlays: Vec<TopologyOverlayDto>,
    ) -> Result<Self, String> {
        Ok(Self {
            schema_version: TOPOLOGY_SCHEMA_VERSION,
            state,
            nodes: topology
                .nodes()
                .iter()
                .map(TopologyNodeDto::project)
                .collect::<Result<_, _>>()?,
            edges: topology.edges().iter().map(Into::into).collect(),
            overlays,
        })
    }
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct TopologyStateDto {
    config: ConfigStateDto,
    runtime: RuntimeStateDto,
    sampled_at_unix_ms: u64,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
enum ConfigStateDto {
    Active,
    Candidate,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
enum RuntimeStateDto {
    Active,
    Degraded,
    Starting,
    NotActive,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum TopologyNodeDto {
    Listener {
        #[serde(flatten)]
        common: TopologyNodeCommonDto,
        attributes: ListenerAttributesDto,
    },
    ForwardProxyListener {
        #[serde(flatten)]
        common: TopologyNodeCommonDto,
        attributes: ListenerConfigAttributesDto,
    },
    ForwardProxyService {
        #[serde(flatten)]
        common: TopologyNodeCommonDto,
        attributes: ForwardProxyServiceAttributesDto,
    },
    RtmpListener {
        #[serde(flatten)]
        common: TopologyNodeCommonDto,
        attributes: RtmpListenerAttributesDto,
    },
    TlsProfile {
        #[serde(flatten)]
        common: TopologyNodeCommonDto,
        attributes: TlsProfileAttributesDto,
    },
    Certificate {
        #[serde(flatten)]
        common: TopologyNodeCommonDto,
        attributes: CertificateAttributesDto,
    },
    HttpService {
        #[serde(flatten)]
        common: TopologyNodeCommonDto,
        attributes: HttpServiceAttributesDto,
    },
    HttpRoute {
        #[serde(flatten)]
        common: TopologyNodeCommonDto,
        attributes: HttpRouteAttributesDto,
    },
    L4Service {
        #[serde(flatten)]
        common: TopologyNodeCommonDto,
        attributes: L4ServiceAttributesDto,
    },
    UpstreamPool {
        #[serde(flatten)]
        common: TopologyNodeCommonDto,
        attributes: UpstreamPoolAttributesDto,
    },
    Endpoint {
        #[serde(flatten)]
        common: TopologyNodeCommonDto,
        attributes: EndpointAttributesDto,
    },
}

impl TopologyNodeDto {
    fn project(node: &TopologyNode) -> Result<Self, String> {
        let common = TopologyNodeCommonDto {
            id: node.id.clone(),
            name: node.name.clone(),
            config_path: node.config_path.clone(),
        };
        macro_rules! node {
            ($variant:ident, $attributes:ty) => {
                Self::$variant {
                    common,
                    attributes: deserialize_attributes::<$attributes>(node)?,
                }
            };
        }
        Ok(match node.kind {
            TopologyNodeKind::Listener => node!(Listener, ListenerAttributesDto),
            TopologyNodeKind::ForwardProxyListener => {
                node!(ForwardProxyListener, ListenerConfigAttributesDto)
            }
            TopologyNodeKind::ForwardProxyService => {
                node!(ForwardProxyService, ForwardProxyServiceAttributesDto)
            }
            TopologyNodeKind::RtmpListener => node!(RtmpListener, RtmpListenerAttributesDto),
            TopologyNodeKind::TlsProfile => node!(TlsProfile, TlsProfileAttributesDto),
            TopologyNodeKind::Certificate => node!(Certificate, CertificateAttributesDto),
            TopologyNodeKind::HttpService => node!(HttpService, HttpServiceAttributesDto),
            TopologyNodeKind::HttpRoute => node!(HttpRoute, HttpRouteAttributesDto),
            TopologyNodeKind::L4Service => node!(L4Service, L4ServiceAttributesDto),
            TopologyNodeKind::UpstreamPool => node!(UpstreamPool, UpstreamPoolAttributesDto),
            TopologyNodeKind::Endpoint => node!(Endpoint, EndpointAttributesDto),
        })
    }
}

fn deserialize_attributes<T: DeserializeOwned>(node: &TopologyNode) -> Result<T, String> {
    serde_json::from_value(node.attributes.clone()).map_err(|error| {
        format!(
            "could not project {} topology node `{}` attributes: {error}",
            node_kind_name(node.kind),
            node.name
        )
    })
}

const fn node_kind_name(kind: TopologyNodeKind) -> &'static str {
    match kind {
        TopologyNodeKind::Listener => "listener",
        TopologyNodeKind::ForwardProxyListener => "forward_proxy_listener",
        TopologyNodeKind::ForwardProxyService => "forward_proxy_service",
        TopologyNodeKind::RtmpListener => "rtmp_listener",
        TopologyNodeKind::TlsProfile => "tls_profile",
        TopologyNodeKind::Certificate => "certificate",
        TopologyNodeKind::HttpService => "http_service",
        TopologyNodeKind::HttpRoute => "http_route",
        TopologyNodeKind::L4Service => "l4_service",
        TopologyNodeKind::UpstreamPool => "upstream_pool",
        TopologyNodeKind::Endpoint => "endpoint",
    }
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct TopologyNodeCommonDto {
    id: String,
    name: String,
    config_path: String,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(untagged)]
enum ListenerAttributesDto {
    Configured(ListenerConfigAttributesDto),
    StatsPage(StatsPageAttributesDto),
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListenerConfigAttributesDto {
    bind: ListenerBindDto,
    protocol: ProtocolDto,
    service: Option<String>,
    tls_profile: Option<String>,
    max_connections: Option<u64>,
    downstream_timeouts: DownstreamTimeoutsDto,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StatsPageAttributesDto {
    bind: ListenerBindDto,
    protocol: HttpProtocolDto,
    max_connections: Option<u64>,
    downstream_timeouts: DownstreamTimeoutsDto,
    uri_prefix: String,
    refresh_ms: u64,
    admin: StatsPageAdminDto,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RtmpListenerAttributesDto {
    bind: ListenerBindDto,
    protocol: RtmpProtocolDto,
    service: Option<String>,
    tls_profile: Option<String>,
    max_connections: Option<u64>,
    downstream_timeouts: DownstreamTimeoutsDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    outbound_chunk_size: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    access_log: Option<AccessLogDto>,
    applications: Vec<RtmpApplicationDto>,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RtmpApplicationDto {
    name: String,
    live: bool,
    idle_streams: bool,
    push_target_count: usize,
    fanout: RtmpFanoutDto,
    recording: RtmpRecordingDto,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(clippy::struct_field_names)]
struct RtmpFanoutDto {
    max_subscribers: u64,
    max_queue_messages_per_subscriber: u64,
    max_queue_bytes_per_subscriber: u64,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RtmpRecordingDto {
    supported: bool,
    recorder_count: usize,
    manual_recorder_count: usize,
    continuous_recorder_count: usize,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)]
struct ForwardProxyServiceAttributesDto {
    enabled_versions: Vec<String>,
    allow_absolute_form: bool,
    tls_required: bool,
    connect_enabled: bool,
    connect_port_count: usize,
    connect_udp_enabled: bool,
    connect_udp_port_count: usize,
    auth: ForwardProxyAuthDto,
    access_rule_count: usize,
    deny_private: bool,
    nameserver_count: usize,
    audit_mode: String,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TlsProfileAttributesDto {
    certificates: Vec<String>,
    default_certificate: String,
    min_version: TlsVersionDto,
    alpn: Vec<AlpnDto>,
    client_auth: TlsClientAuthDto,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TlsClientAuthDto {
    mode: TlsClientAuthModeDto,
    ca_configured: bool,
    allowed_dns_name_count: usize,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CertificateAttributesDto {
    dns_names: Vec<String>,
    source: CertificateSourceDto,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum CertificateSourceDto {
    Files {
        certificate_chain_path: PathBuf,
        private_key_path: RedactedPrivateKeyDto,
    },
    Certbot {
        live_directory_path: PathBuf,
        archive_directory_path: PathBuf,
    },
    AcmeManaged {
        directory_url: String,
        state_root: PathBuf,
        contact_count: usize,
        terms_agreed: bool,
        challenge: String,
        key_type: KeyTypeDto,
        allowed_dns_suffix_count: usize,
        retained_revisions: u32,
        retention_days: u32,
        dns_provider: Option<String>,
    },
    SelfSignedDevelopment {
        development_only: bool,
        validity_days: u32,
        key_type: KeyTypeDto,
    },
}

#[derive(Deserialize, JsonSchema, Serialize)]
enum RedactedPrivateKeyDto {
    #[serde(rename = "<redacted>")]
    Redacted,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HttpServiceAttributesDto {
    upstream_io_timeout_ms: u64,
    max_request_body_bytes: Option<u64>,
    gzip: Option<HttpGzipDto>,
    access_log: AccessLogDto,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HttpGzipDto {
    level: u8,
    content_types: Vec<String>,
    min_length_bytes: u64,
    min_http_version: String,
    disable_on_via: bool,
    vary: bool,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HttpRouteAttributesDto {
    host: Option<HttpHostSelectorDto>,
    path: HttpPathSelectorDto,
    methods: Vec<String>,
    access: Option<HttpAccessPolicyDto>,
    policy: HttpRoutePolicyDto,
    action: HttpRouteActionDto,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum HttpHostSelectorDto {
    NormalizedHost { value: String },
    ExactAuthority { value: String },
    AsciiCaseInsensitiveExactAuthority { value: String },
    NginxLeadingWildcard { value: String },
    NginxLeadingDot { value: String },
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum HttpPathSelectorDto {
    SegmentPrefix { value: String },
    RawPrefix { value: String },
    Exact { value: String },
    AsciiCaseInsensitiveExact { value: String },
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum HttpAccessPolicyDto {
    BearerTokenFile {
        header_name: String,
        realm: Option<String>,
    },
    BasicHtpasswdFile {
        realm: Option<String>,
    },
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HttpRoutePolicyDto {
    max_request_body_bytes: Option<u64>,
    connect_timeout_ms: u64,
    read_timeout_ms: u64,
    write_timeout_ms: u64,
    request_buffering: bool,
    response_buffering: bool,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum HttpRouteActionDto {
    Proxy {
        upstream_pool: String,
        upstream_host: String,
        request_header_mutation_count: usize,
        response_header_mutation_count: usize,
        cookie_path_rewrite_count: usize,
        cookie_attribute_policy_count: usize,
        retry: HttpRetryDto,
    },
    FixedResponse {
        status: u16,
        body_bytes: usize,
        header_count: usize,
    },
    Redirect {
        status: u16,
        location_type: String,
        header_count: usize,
    },
    StaticFiles {
        path_mapping: String,
        index_files: Vec<String>,
        spa_fallback: bool,
        try_file_count: usize,
        autoindex: bool,
        autoindex_exact_size: bool,
        autoindex_local_time: bool,
        mime_mapping_count: usize,
        default_type: String,
        header_count: usize,
        error_response_count: usize,
    },
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HttpRetryDto {
    max_retries: u8,
    triggers: Vec<String>,
    target: String,
    delay_ms: u64,
    final_redispatch: bool,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct L4ServiceAttributesDto {
    upstream_pool: String,
    connect_timeout_ms: u64,
    idle_timeout_ms: u64,
    lifetime_timeout_ms: Option<u64>,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpstreamPoolAttributesDto {
    algorithm: UpstreamAlgorithmDto,
    health_check: Option<HealthCheckDto>,
    tls: Option<UpstreamTlsDto>,
    http_versions: HttpVersionsDto,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum HealthCheckDto {
    Http {
        interval_ms: u64,
        timeout_ms: u64,
        healthy_threshold: u16,
        unhealthy_threshold: u16,
        host: Option<String>,
        path: Option<String>,
    },
    Tcp {
        interval_ms: u64,
        timeout_ms: u64,
        healthy_threshold: u16,
        unhealthy_threshold: u16,
        host: Option<String>,
        path: Option<String>,
    },
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpstreamTlsDto {
    server_name: String,
    ca_certificate_path: Option<PathBuf>,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HttpVersionsDto {
    min: HttpVersionDto,
    max: HttpVersionDto,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum EndpointAttributesDto {
    Socket {
        address: String,
        max_connections: Option<u64>,
        server_name: String,
    },
    Dns {
        host: String,
        port: u16,
        address: String,
        max_connections: Option<u64>,
        server_name: String,
    },
    Unix {
        path: PathBuf,
        address: String,
        max_connections: Option<u64>,
        server_name: String,
    },
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum ListenerBindDto {
    Socket { address: String },
    Udp { address: String },
    Unix { path: PathBuf, mode: Option<u16> },
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(clippy::struct_field_names)]
struct DownstreamTimeoutsDto {
    client_timeout_ms: Option<u64>,
    request_timeout_ms: Option<u64>,
    keepalive_timeout_ms: Option<u64>,
}

macro_rules! string_enum {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Deserialize, JsonSchema, Serialize)]
        #[serde(rename_all = "snake_case")]
        enum $name {
            $($variant),+
        }
    };
}

string_enum!(ProtocolDto {
    Http,
    Tcp,
    Udp,
    ForwardHttp1,
    ForwardHttp2,
    ForwardHttp3,
    Http3,
});
string_enum!(HttpProtocolDto { Http });
string_enum!(RtmpProtocolDto { Rtmp });
string_enum!(StatsPageAdminDto {
    Disabled,
    Localhost,
});
string_enum!(AccessLogDto {
    Disabled,
    StructuredFile,
    DefaultDisabled,
});
string_enum!(ForwardProxyAuthDto {
    BearerTokenFile,
    BasicHtpasswdFile,
    MutualTls,
    None,
});
string_enum!(TlsClientAuthModeDto {
    Disabled,
    Optional,
    Required,
});
string_enum!(KeyTypeDto { EcdsaP256, Rsa2048 });
string_enum!(UpstreamAlgorithmDto {
    RoundRobin,
    WeightedRoundRobin,
    LeastConnections,
    First,
});

#[derive(Deserialize, JsonSchema, Serialize)]
enum TlsVersionDto {
    #[serde(rename = "1.2")]
    Tls12,
    #[serde(rename = "1.3")]
    Tls13,
}

#[derive(Deserialize, JsonSchema, Serialize)]
enum AlpnDto {
    #[serde(rename = "h3")]
    H3,
    #[serde(rename = "h2")]
    H2,
    #[serde(rename = "http/1.1")]
    Http11,
}

#[derive(Deserialize, JsonSchema, Serialize)]
enum HttpVersionDto {
    #[serde(rename = "1.1")]
    Http11,
    #[serde(rename = "2")]
    Http2,
    #[serde(rename = "3")]
    Http3,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct TopologyEdgeDto {
    id: String,
    kind: TopologyEdgeKindDto,
    source: String,
    target: String,
    config_path: String,
}

impl From<&TopologyEdge> for TopologyEdgeDto {
    fn from(edge: &TopologyEdge) -> Self {
        Self {
            id: edge.id.clone(),
            kind: edge.kind.into(),
            source: edge.source.clone(),
            target: edge.target.clone(),
            config_path: edge.config_path.clone(),
        }
    }
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
enum TopologyEdgeKindDto {
    DispatchService,
    ServiceRoute,
    RoutePool,
    ServicePool,
    PoolEndpoint,
    ListenerTls,
    TlsCertificate,
}

impl From<TopologyEdgeKind> for TopologyEdgeKindDto {
    fn from(kind: TopologyEdgeKind) -> Self {
        match kind {
            TopologyEdgeKind::DispatchService => Self::DispatchService,
            TopologyEdgeKind::ServiceRoute => Self::ServiceRoute,
            TopologyEdgeKind::RoutePool => Self::RoutePool,
            TopologyEdgeKind::ServicePool => Self::ServicePool,
            TopologyEdgeKind::PoolEndpoint => Self::PoolEndpoint,
            TopologyEdgeKind::ListenerTls => Self::ListenerTls,
            TopologyEdgeKind::TlsCertificate => Self::TlsCertificate,
        }
    }
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(untagged)]
enum TopologyOverlayDto {
    Listener(TopologyOverlay<ListenerOverlayStateDto, ListenerOverlayMetricsDto>),
    Pool(TopologyOverlay<PoolOverlayStateDto, PoolOverlayMetricsDto>),
    Endpoint(TopologyOverlay<EndpointOverlayStateDto, EndpointOverlayMetricsDto>),
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct TopologyOverlay<S, M> {
    node_id: String,
    state: S,
    metrics: M,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
enum ListenerOverlayStateDto {
    Configured,
    Listening,
    Stopped,
    Failed,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
enum PoolOverlayStateDto {
    Available,
    Degraded,
    Unavailable,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
enum EndpointOverlayStateDto {
    Unchecked,
    Unknown,
    Healthy,
    Unhealthy,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct ListenerOverlayMetricsDto {
    active_connections: u64,
    accepted_connections: DecimalCounter,
    rejected_connections: DecimalCounter,
    bytes_received: DecimalCounter,
    bytes_sent: DecimalCounter,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct PoolOverlayMetricsDto {
    available_endpoints: usize,
    total_endpoints: usize,
    unavailable_selections: DecimalCounter,
    queued: u64,
    queued_total: DecimalCounter,
    queue_timeouts: DecimalCounter,
    queue_cancellations: DecimalCounter,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct EndpointOverlayMetricsDto {
    active_connections: DecimalCounter,
    max_connections: Option<u64>,
    last_checked_at_unix_ms: Option<u64>,
    last_transition_at_unix_ms: Option<u64>,
    successful_checks: DecimalCounter,
    failed_checks: DecimalCounter,
    consecutive_successes: DecimalCounter,
    consecutive_failures: DecimalCounter,
    last_failure: Option<HealthFailureDto>,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
enum HealthFailureDto {
    Timeout,
    ConnectFailed,
    UnexpectedStatus,
    ProtocolError,
}

#[cfg(test)]
mod tests {
    use schemars::generate::SchemaSettings;
    use serde_json::{Value, json};

    use super::*;

    fn empty_topology() -> TopologySnapshot {
        TopologySnapshot::compile(&oxiroute_config::ConfigDraft {
            version: 1,
            max_connections: None,
            management: None,
            stats: None,
            certificates: Vec::new(),
            tls_profiles: Vec::new(),
            listeners: Vec::new(),
            cache_stores: Vec::new(),
            upstream_pools: Vec::new(),
            http_services: Vec::new(),
            forward_proxy_services: Vec::new(),
            rtmp_services: Vec::new(),
            l4_services: Vec::new(),
        })
    }

    fn schema() -> Value {
        let generator = SchemaSettings::draft2020_12().into_generator();
        serde_json::to_value(generator.into_root_schema_for::<TopologyResponse>())
            .expect("topology response schema")
    }

    #[test]
    fn active_and_candidate_projections_preserve_the_v1_golden_objects() {
        let topology = empty_topology();
        let runtime = RuntimeHealthSnapshot {
            sampled_at_unix_ms: 42,
            listeners: Vec::new(),
            upstream_pools: Vec::new(),
        };
        let active = serde_json::to_value(
            TopologyResponse::active(&topology, &runtime).expect("active topology projection"),
        )
        .expect("active topology JSON");
        let candidate = serde_json::to_value(
            TopologyResponse::candidate(&topology, 43).expect("candidate topology projection"),
        )
        .expect("candidate topology JSON");

        assert_eq!(
            active,
            json!({
                "schemaVersion": 1,
                "state": {
                    "config": "active",
                    "runtime": "active",
                    "sampledAtUnixMs": 42,
                },
                "nodes": [],
                "edges": [],
                "overlays": [],
            })
        );
        assert_eq!(
            candidate,
            json!({
                "schemaVersion": 1,
                "state": {
                    "config": "candidate",
                    "runtime": "not_active",
                    "sampledAtUnixMs": 43,
                },
                "nodes": [],
                "edges": [],
                "overlays": [],
            })
        );
        assert_eq!(
            active,
            topology
                .response_value(&runtime)
                .expect("legacy active topology JSON")
        );
    }

    #[test]
    fn topology_schema_is_structural_and_discriminates_nodes_and_overlays() {
        let schema = schema();
        let encoded = schema.to_string();

        assert!(!encoded.contains("serde_json::Value"));
        assert!(!encoded.contains("\"additionalProperties\":true"));
        for kind in [
            "listener",
            "forward_proxy_listener",
            "forward_proxy_service",
            "rtmp_listener",
            "tls_profile",
            "certificate",
            "http_service",
            "http_route",
            "l4_service",
            "upstream_pool",
            "endpoint",
        ] {
            assert!(encoded.contains(&format!("\"const\":\"{kind}\"")));
        }
        for metrics in [
            "ListenerOverlayMetricsDto",
            "PoolOverlayMetricsDto",
            "EndpointOverlayMetricsDto",
        ] {
            assert!(encoded.contains(metrics), "schema omitted {metrics}");
        }
        assert_eq!(schema["$defs"]["DecimalCounter"]["type"], "string");
    }

    #[test]
    fn topology_node_projection_is_exhaustive_over_domain_kinds() {
        let names = [
            TopologyNodeKind::Listener,
            TopologyNodeKind::ForwardProxyListener,
            TopologyNodeKind::ForwardProxyService,
            TopologyNodeKind::RtmpListener,
            TopologyNodeKind::TlsProfile,
            TopologyNodeKind::Certificate,
            TopologyNodeKind::HttpService,
            TopologyNodeKind::HttpRoute,
            TopologyNodeKind::L4Service,
            TopologyNodeKind::UpstreamPool,
            TopologyNodeKind::Endpoint,
        ]
        .map(node_kind_name);

        assert_eq!(
            names,
            [
                "listener",
                "forward_proxy_listener",
                "forward_proxy_service",
                "rtmp_listener",
                "tls_profile",
                "certificate",
                "http_service",
                "http_route",
                "l4_service",
                "upstream_pool",
                "endpoint",
            ]
        );
    }

    #[test]
    fn topology_schema_excludes_secret_bearing_source_fields() {
        let schema = schema().to_string().to_ascii_lowercase();
        for forbidden in [
            "tokenfilepath",
            "htpasswdfilepath",
            "rootdirectory",
            "contacts",
            "accountkey",
            "credentials",
        ] {
            assert!(!schema.contains(forbidden), "schema exposed {forbidden}");
        }
    }
}
