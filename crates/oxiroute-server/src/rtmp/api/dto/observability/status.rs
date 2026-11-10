#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StatusResponse {
    schema_version: u8,
    build_version: &'static str,
    disk_revision: Option<String>,
    candidate_revision: Option<String>,
    active_revision: Option<String>,
    previous_revision: Option<String>,
    degraded: bool,
    active_generation_age_ms: u64,
    components: StatusComponentsDto,
    certificates: StatusCertificatesDto,
    audit: AuditStatusDto,
    capabilities: CapabilitiesResponse,
    listeners: Vec<ListenerDto>,
    tls_profiles: Vec<TlsProfileDto>,
}

impl StatusResponse {
    pub(crate) fn project(
        runtime: RuntimeSnapshot,
        generation: Option<&GenerationStatus>,
        audit: AuditStatus,
        topology: &TopologySnapshot,
        mode: RuntimeMode,
    ) -> Result<Self, String> {
        let capabilities = CapabilitiesResponse::project(&runtime.listeners, mode)
            .map_err(|error| error.to_string())?;
        let listeners = runtime.listeners.into_iter().map(Into::into).collect();
        Ok(Self {
            schema_version: 1,
            build_version: crate::cli::BUILD_VERSION,
            disk_revision: generation
                .and_then(|status| status.disk_revision.as_ref())
                .map(|revision| revision.as_str().to_owned()),
            candidate_revision: generation
                .and_then(|status| status.candidate_revision.as_ref())
                .map(|revision| revision.as_str().to_owned()),
            active_revision: generation
                .and_then(|status| status.active_revision.as_ref())
                .map(|revision| revision.as_str().to_owned()),
            previous_revision: generation
                .and_then(|status| status.previous_revision.as_ref())
                .map(|revision| revision.as_str().to_owned()),
            degraded: generation.is_none_or(|status| status.degraded),
            active_generation_age_ms: runtime.generation_age_ms,
            components: StatusComponentsDto {
                process: runtime.process.status.into(),
                host: runtime.host.status.into(),
                generation: generation_component(generation),
                audit: audit.clone().into(),
            },
            certificates: StatusCertificatesDto {
                certbot: runtime
                    .certbot_certificates
                    .into_iter()
                    .map(Into::into)
                    .collect(),
                acme_managed: runtime
                    .acme_managed_certificates
                    .into_iter()
                    .map(Into::into)
                    .collect(),
                direct_files: runtime
                    .direct_file_certificates
                    .into_iter()
                    .map(Into::into)
                    .collect(),
            },
            audit: audit.into(),
            capabilities,
            listeners,
            tls_profiles: topology
                .nodes()
                .iter()
                .filter(|node| node.kind == TopologyNodeKind::TlsProfile)
                .map(tls_profile)
                .collect::<Result<_, _>>()?,
        })
    }
}

fn tls_profile(node: &crate::TopologyNode) -> Result<TlsProfileDto, String> {
    let client_auth = node
        .attributes
        .get("clientAuth")
        .ok_or_else(|| "TLS profile is missing clientAuth".to_owned())?;
    Ok(TlsProfileDto {
        name: node.name.clone(),
        client_auth: TlsClientAuthDto {
            mode: client_auth
                .get("mode")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "TLS clientAuth is missing mode".to_owned())?
                .to_owned(),
            ca_configured: client_auth
                .get("caConfigured")
                .and_then(serde_json::Value::as_bool)
                .ok_or_else(|| "TLS clientAuth is missing caConfigured".to_owned())?,
            allowed_dns_name_count: client_auth
                .get("allowedDnsNameCount")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| "TLS clientAuth is missing allowedDnsNameCount".to_owned())?,
        },
    })
}

#[derive(JsonSchema, Serialize)]
struct StatusComponentsDto {
    process: ComponentStatusDto,
    host: ComponentStatusDto,
    generation: ComponentStatusDto,
    audit: AuditStatusDto,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusCertificatesDto {
    certbot: Vec<CertbotCertificateDto>,
    acme_managed: Vec<AcmeManagedCertificateDto>,
    direct_files: Vec<DirectFileCertificateDto>,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct TlsProfileDto {
    name: String,
    client_auth: TlsClientAuthDto,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct TlsClientAuthDto {
    mode: String,
    ca_configured: bool,
    allowed_dns_name_count: u64,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReadinessResponse {
    ready: bool,
    build_version: &'static str,
    active_revision: Option<String>,
}

impl ReadinessResponse {
    pub(crate) fn project(serving: bool, generation: Option<GenerationStatus>) -> Self {
        let ready = serving && generation.as_ref().is_some_and(|status| status.serving());
        Self {
            ready,
            build_version: crate::cli::BUILD_VERSION,
            active_revision: generation
                .and_then(|status| status.active_revision)
                .map(|revision| revision.as_str().to_owned()),
        }
    }

    pub(crate) const fn ready(&self) -> bool {
        self.ready
    }
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CapabilitiesResponse {
    schema_version: u8,
    supervision: SupervisionCapabilityDto,
    udp: UdpCapabilityDto,
    http3: Http3CapabilitiesDto,
}

impl CapabilitiesResponse {
    pub(crate) fn project(
        listeners: &[ListenerSnapshot],
        mode: RuntimeMode,
    ) -> Result<Self, serde_json::Error> {
        serde_json::from_value(crate::http3::capability_snapshot(listeners, mode))
    }
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
struct SupervisionCapabilityDto {
    mode: RuntimeModeDto,
    #[serde(rename = "descriptorAdoption")]
    descriptor_adoption: DescriptorAdoptionDto,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DescriptorAdoptionDto {
    status: DescriptorAdoptionStatusDto,
    manifest_version: u8,
    datagram: bool,
    quic: bool,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
struct Http3CapabilitiesDto {
    reverse: H3CapabilityDto,
    forward: H3CapabilityDto,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UdpCapabilityDto {
    status: CapabilityStatusDto,
    supported: bool,
    listeners: Vec<String>,
    configured_listeners: Vec<String>,
    transport: UdpTransportDto,
    drain: GracefulDto,
    fallback: NoneDto,
    blocked_reason: Option<BlockedReasonDto>,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct H3CapabilityDto {
    status: CapabilityStatusDto,
    supported: bool,
    listeners: Vec<String>,
    configured_listeners: Vec<String>,
    transport: QuicTransportDto,
    alpn: Vec<String>,
    tls_min_version: String,
    zero_rtt: DisabledDto,
    migration: DisabledDto,
    go_away: GracefulDto,
    fallback: NoneDto,
    unsupported: Vec<String>,
    limits: H3LimitsDto,
    blocked_reason: Option<BlockedReasonDto>,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(clippy::struct_field_names)]
struct H3LimitsDto {
    max_handshakes_and_connections: usize,
    max_bidirectional_streams: u32,
    max_unidirectional_streams: u32,
    max_field_section_bytes: u64,
    max_request_body_bytes: u64,
    max_response_body_bytes: u64,
}

macro_rules! string_enum {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Deserialize, JsonSchema, Serialize)]
        #[serde(rename_all = "snake_case")]
        #[allow(clippy::enum_variant_names)]
        enum $name {
            $($variant),+
        }
    };
}

string_enum!(RuntimeModeDto { Direct, Supervised });
string_enum!(DescriptorAdoptionStatusDto {
    Negotiated,
    NotUsed
});
string_enum!(CapabilityStatusDto {
    Active,
    Unconfigured,
    Blocked
});
string_enum!(BlockedReasonDto {
    ListenerRuntimeFailed,
    ListenerStopped,
    ListenerNotListening,
});
string_enum!(UdpTransportDto { Udp });
string_enum!(QuicTransportDto { Quic });
string_enum!(GracefulDto { Graceful });
string_enum!(NoneDto { None });
string_enum!(DisabledDto { Disabled });

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct AuditStatusDto {
    state: AuditComponentStateDto,
    persistent: bool,
    degraded: bool,
    record_count: u64,
    bytes: u64,
    rotated_files: u64,
    max_records: u64,
    max_record_bytes: u64,
    max_file_bytes: u64,
    max_total_bytes: u64,
    max_rotated_files: u64,
    write_failures: u64,
    corrupt_records: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_error: Option<&'static str>,
}

impl From<AuditStatus> for AuditStatusDto {
    fn from(status: AuditStatus) -> Self {
        Self {
            state: status.state.into(),
            persistent: status.persistent,
            degraded: status.degraded,
            record_count: status.record_count,
            bytes: status.bytes,
            rotated_files: status.rotated_files,
            max_records: status.max_records,
            max_record_bytes: status.max_record_bytes,
            max_file_bytes: status.max_file_bytes,
            max_total_bytes: status.max_total_bytes,
            max_rotated_files: status.max_rotated_files,
            write_failures: status.write_failures,
            corrupt_records: status.corrupt_records,
            last_error: status.last_error,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ComponentStatusDto {
    state: ComponentStateDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

impl From<ComponentStatus> for ComponentStatusDto {
    fn from(status: ComponentStatus) -> Self {
        Self {
            state: status.state.into(),
            reason: status.reason,
        }
    }
}

fn generation_component(generation: Option<&GenerationStatus>) -> ComponentStatusDto {
    if let Some(status) = generation.filter(|status| status.degraded) {
        ComponentStatusDto {
            state: ComponentStateDto::Degraded,
            reason: status.last_failure,
        }
    } else if generation.is_some_and(|status| status.active_revision.is_some()) {
        ComponentStatusDto {
            state: ComponentStateDto::Healthy,
            reason: None,
        }
    } else {
        ComponentStatusDto {
            state: ComponentStateDto::Degraded,
            reason: Some("active_generation_unavailable"),
        }
    }
}
