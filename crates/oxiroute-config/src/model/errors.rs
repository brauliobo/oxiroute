use super::*;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Lua configuration failed: {0}")]
    Lua(#[from] mlua::Error),
    #[error("configuration exceeds the {MAX_SOURCE_BYTES}-byte source limit")]
    SourceTooLarge,
    #[error("unsupported configuration version {0}; expected version 1")]
    UnsupportedVersion(u32),
    #[error("{namespace} at index {index} has a blank name")]
    BlankName {
        namespace: &'static str,
        index: usize,
    },
    #[error("{namespace} at index {index} has noncanonical name {name:?}")]
    InvalidName {
        namespace: &'static str,
        index: usize,
        name: String,
    },
    #[error("duplicate {namespace} name `{name}`")]
    DuplicateName {
        namespace: &'static str,
        name: String,
    },
    #[error("configuration exceeds the {MAX_CERTIFICATES}-certificate limit")]
    TooManyCertificates,
    #[error("certificate `{certificate}` must declare at least one DNS name")]
    EmptyCertificateDnsNames { certificate: String },
    #[error("certificate `{certificate}` exceeds the {MAX_CERTIFICATE_DNS_NAMES}-DNS-name limit")]
    TooManyCertificateDnsNames { certificate: String },
    #[error("certificate `{certificate}` has invalid DNS name `{dns_name}`")]
    InvalidCertificateDnsName {
        certificate: String,
        dns_name: String,
    },
    #[error("certificate `{certificate}` contains duplicate DNS name `{dns_name}`")]
    DuplicateCertificateDnsName {
        certificate: String,
        dns_name: String,
    },
    #[error("configuration exceeds the {MAX_TLS_PROFILES}-TLS-profile limit")]
    TooManyTlsProfiles,
    #[error("{kind} `{name}` has invalid `{field}`: {detail}")]
    InvalidFilePath {
        kind: &'static str,
        name: String,
        field: &'static str,
        detail: &'static str,
    },
    #[error("certificate `{certificate}` must use different chain and private-key paths")]
    DuplicateCertificatePaths { certificate: String },
    #[error("certificate `{certificate}` must use different Certbot live and archive directories")]
    DuplicateCertbotDirectories { certificate: String },
    #[error("managed ACME certificate `{certificate}` has an invalid HTTPS directory URL")]
    InvalidAcmeDirectoryUrl { certificate: String },
    #[error("managed ACME certificate `{certificate}` must explicitly agree to directory terms")]
    AcmeTermsNotAgreed { certificate: String },
    #[error("managed ACME certificate `{certificate}` uses an unsupported challenge type")]
    UnsupportedAcmeChallenge { certificate: String },
    #[error("managed ACME certificate `{certificate}` wildcard `{dns_name}` requires DNS-01")]
    AcmeWildcardRequiresDns01 {
        certificate: String,
        dns_name: String,
    },
    #[error("managed ACME certificate `{certificate}` has an invalid DNS-01 provider")]
    InvalidAcmeDns01Provider { certificate: String },
    #[error("managed ACME certificate `{certificate}` has an invalid DNS-01 credential file")]
    InvalidAcmeDns01Credentials { certificate: String },
    #[error("managed ACME certificate `{certificate}` has an invalid DNS-01 timeout")]
    InvalidAcmeDns01Timeout { certificate: String },
    #[error("managed ACME certificate `{certificate}` has invalid contacts")]
    InvalidAcmeContacts { certificate: String },
    #[error("managed ACME certificate `{certificate}` has invalid revision retention")]
    InvalidAcmeRetention { certificate: String },
    #[error(
        "managed ACME certificate `{certificate}` must configure between one and sixteen DNS suffixes"
    )]
    InvalidAcmeDnsSuffixes { certificate: String },
    #[error("managed ACME certificate `{certificate}` contains an IP identifier")]
    AcmeIdentifierUnsupported { certificate: String },
    #[error("managed ACME certificate `{certificate}` name must be a path-safe slug")]
    InvalidAcmeCertificateName { certificate: String },
    #[error(
        "managed ACME certificate `{certificate}` DNS name `{dns_name}` is outside its suffix policy"
    )]
    AcmeIdentifierOutsidePolicy {
        certificate: String,
        dns_name: String,
    },
    #[error(
        "development certificate `{certificate}` validity_days must be between {min} and {max}, got {value}"
    )]
    InvalidSelfSignedValidityDays {
        certificate: String,
        value: u32,
        min: u32,
        max: u32,
    },
    #[error("TLS profile `{profile}` references unknown certificate `{certificate}`")]
    UnknownTlsProfileCertificate {
        profile: String,
        certificate: String,
    },
    #[error("TLS profile `{profile}` must reference at least one certificate")]
    EmptyTlsProfileCertificates { profile: String },
    #[error("TLS profile `{profile}` references certificate `{certificate}` more than once")]
    DuplicateTlsProfileCertificate {
        profile: String,
        certificate: String,
    },
    #[error(
        "TLS profile `{profile}` default certificate `{certificate}` is not in its certificate list"
    )]
    TlsProfileDefaultNotListed {
        profile: String,
        certificate: String,
    },
    #[error(
        "TLS profile `{profile}` assigns DNS name `{dns_name}` to both `{first_certificate}` and `{second_certificate}`"
    )]
    OverlappingTlsProfileDnsName {
        profile: String,
        dns_name: String,
        first_certificate: String,
        second_certificate: String,
    },
    #[error(
        "TLS profile `{profile}` has invalid ALPN policy; expected [http/1.1], [h2], [h2, http/1.1], or [h3]"
    )]
    InvalidTlsProfileAlpn { profile: String },
    #[error("TLS profile `{profile}` has invalid `{field}` policy: {detail}")]
    InvalidTlsProfilePolicy {
        profile: String,
        field: &'static str,
        detail: &'static str,
    },
    #[error("TLS profile `{profile}` has invalid client-auth DNS name `{dns_name}`")]
    InvalidTlsClientAuthDnsName { profile: String, dns_name: String },
    #[error("TLS profile `{profile}` contains duplicate client-auth DNS name `{dns_name}`")]
    DuplicateTlsClientAuthDnsName { profile: String, dns_name: String },
    #[error("TLS profile `{profile}` exceeds the 100-client-auth-DNS-name limit")]
    TooManyTlsClientAuthDnsNames { profile: String },
    #[error("binds `{first_name}` ({first_bind}) and `{second_name}` ({second_bind}) overlap")]
    OverlappingBind {
        first_name: String,
        first_bind: Box<ListenerBind>,
        second_name: String,
        second_bind: Box<ListenerBind>,
    },
    #[error("{kind} `{name}` has an invalid zero port in `{field}`")]
    ZeroPort {
        kind: &'static str,
        name: String,
        field: &'static str,
    },
    #[error("{kind} `{name}` must have a nonzero `{field}`")]
    ZeroLimit {
        kind: &'static str,
        name: String,
        field: &'static str,
    },
    #[error("{kind} `{name}` exceeds the exact JSON integer limit in `{field}`")]
    LimitTooLarge {
        kind: &'static str,
        name: String,
        field: &'static str,
    },
    #[error("{protocol:?} listener `{listener}` requires a service")]
    MissingListenerService {
        listener: String,
        protocol: Protocol,
    },
    #[error("{protocol:?} listener `{listener}` references unknown same-kind service `{service}`")]
    UnknownListenerService {
        listener: String,
        protocol: Protocol,
        service: String,
    },
    #[error("listener `{listener}` references unknown TLS profile `{profile}`")]
    UnknownListenerTlsProfile { listener: String, profile: String },
    #[error("{protocol:?} listener `{listener}` must not use TLS profile `{profile}`")]
    UnexpectedListenerTlsProfile {
        listener: String,
        protocol: Protocol,
        profile: String,
    },
    #[error("{protocol:?} listener `{listener}` has invalid transport: {detail}")]
    InvalidListenerTransport {
        listener: String,
        protocol: Protocol,
        detail: &'static str,
    },
    #[error(
        "listener `{listener}` has invalid Unix socket mode {mode:o}; expected permission bits from 001 through 777"
    )]
    InvalidListenerUnixMode { listener: String, mode: u16 },
    #[error("upstream pool `{pool}` must contain at least one endpoint")]
    EmptyUpstreamEndpoints { pool: String },
    #[error("upstream pool `{pool}` exceeds the {MAX_ENDPOINTS_PER_POOL}-endpoint limit")]
    TooManyUpstreamEndpoints { pool: String },
    #[error("configuration exceeds the {MAX_TOTAL_ENDPOINTS}-upstream-endpoint limit")]
    TooManyTotalUpstreamEndpoints,
    #[error("upstream pool `{pool}` contains duplicate endpoint `{endpoint}`")]
    DuplicateUpstreamEndpoint {
        pool: String,
        endpoint: UpstreamEndpoint,
    },
    #[error("upstream pool `{pool}` server `{server}` has invalid `{field}`: {detail}")]
    InvalidUpstreamServer {
        pool: String,
        server: String,
        field: &'static str,
        detail: &'static str,
    },
    #[error("upstream pool `{pool}` has invalid weighted round-robin weights: {detail}")]
    InvalidUpstreamWeights { pool: String, detail: &'static str },
    #[error("upstream pool `{pool}` exposes the loopback management endpoint `{endpoint}`")]
    ManagementUpstreamEndpoint { pool: String, endpoint: SocketAddr },
    #[error("upstream pool `{pool}` has invalid DNS endpoint `{host}`")]
    InvalidDnsEndpoint { pool: String, host: String },
    #[error("{kind} `{name}` has invalid Unix socket `{field}`: {detail}")]
    InvalidUnixPath {
        kind: &'static str,
        name: String,
        field: &'static str,
        detail: &'static str,
    },
    #[error("upstream pool `{pool}` has an invalid health check: {detail}")]
    InvalidHealthCheck { pool: String, detail: &'static str },
    #[error("upstream pool `{pool}` has invalid TLS server name `{server_name}`")]
    InvalidUpstreamTlsServerName { pool: String, server_name: String },
    #[error(
        "upstream pool `{pool}` has invalid HTTP version range {min}/{max}; expected 1.1/1.1, 1.1/2, 2/2, or 3/3"
    )]
    InvalidHttpVersionRange {
        pool: String,
        min: &'static str,
        max: &'static str,
    },
    #[error("upstream pool `{pool}` enables HTTP/2 without TLS; plaintext h2c is not supported")]
    H2RequiresUpstreamTls { pool: String },
    #[error("upstream pool `{pool}` enables HTTP/3 without TLS")]
    H3RequiresUpstreamTls { pool: String },
    #[error("upstream pool `{pool}` combines `health_check` with `tls`, which is not supported")]
    UnsupportedTlsHealthCheck { pool: String },
    #[error("listener `{listener}` cannot terminate TLS profile `{profile}` on a Unix socket")]
    UnsupportedUnixListenerTls { listener: String, profile: String },
    #[error("upstream pool `{pool}` cannot use TLS with a Unix endpoint")]
    UnsupportedUnixUpstreamTls { pool: String },
    #[error("upstream pool `{pool}` cannot health-check a Unix endpoint")]
    UnsupportedUnixHealthCheck { pool: String },
    #[error("HTTP service `{service}` must contain at least one route")]
    EmptyHttpRoutes { service: String },
    #[error("HTTP service `{service}` route {route} has invalid `{field}`: {detail}")]
    InvalidHttpRoute {
        service: String,
        route: usize,
        field: &'static str,
        detail: String,
    },
    #[error(
        "HTTP service `{service}` route {route} uses endpoint Host policy with a Unix endpoint but has no `unix_fallback`"
    )]
    HttpEndpointHostRequiresUnixFallback { service: String, route: usize },
    #[error("configuration exceeds the {MAX_RTMP_SERVICES}-RTMP-service limit")]
    TooManyRtmpServices,
    #[error("RTMP service `{service}` must contain at least one application")]
    EmptyRtmpApplications { service: String },
    #[error(
        "RTMP service `{service}` exceeds the {MAX_RTMP_APPLICATIONS_PER_SERVICE}-application limit"
    )]
    TooManyRtmpApplications { service: String },
    #[error(
        "RTMP application `{application}` in service `{service}` exceeds the {MAX_RTMP_RECORDERS_PER_APPLICATION}-recorder limit"
    )]
    TooManyRtmpRecorders {
        service: String,
        application: String,
    },
    #[error("configuration exceeds the {MAX_TOTAL_RTMP_RECORDERS}-RTMP-recorder limit")]
    TooManyTotalRtmpRecorders,
    #[error("configuration exceeds the {MAX_RTMP_RECORDING_ROOTS}-recording-root limit")]
    TooManyRtmpRecordingRoots,
    #[error("configuration exceeds the {MAX_RTMP_HLS_OUTPUTS}-HLS-output limit")]
    TooManyRtmpHlsOutputs,
    #[error("configuration exceeds the {MAX_RTMP_HLS_OUTPUTS}-HLS-root limit")]
    TooManyRtmpHlsRoots,
    #[error(
        "RTMP HLS outputs `{first_output}` and `{second_output}` use shared media root `{root_directory}` and must use identical storage limits"
    )]
    RtmpHlsStorageLimitsMismatch {
        root_directory: String,
        first_output: String,
        second_output: String,
    },
    #[error("configuration exceeds the {MAX_RTMP_DASH_OUTPUTS}-DASH-output limit")]
    TooManyRtmpDashOutputs,
    #[error("configuration exceeds the {MAX_RTMP_DASH_OUTPUTS}-DASH-root limit")]
    TooManyRtmpDashRoots,
    #[error(
        "RTMP DASH outputs `{first_output}` and `{second_output}` use shared media root `{root_directory}` and must use identical storage limits"
    )]
    RtmpDashStorageLimitsMismatch {
        root_directory: String,
        first_output: String,
        second_output: String,
    },
    #[error(
        "RTMP recorder `{recorder}` in application `{application}` of service `{service}` requires `live = true`"
    )]
    RtmpRecorderRequiresLiveApplication {
        service: String,
        application: String,
        recorder: String,
    },
    #[error(
        "RTMP recorder `{recorder}` in application `{application}` of service `{service}` has invalid `{field}`: {detail}"
    )]
    InvalidRtmpRecorderPolicy {
        service: String,
        application: String,
        recorder: String,
        field: &'static str,
        detail: &'static str,
    },
    #[error("RTMP service `{service}` has invalid `{field}`: {detail}")]
    InvalidRtmpServicePolicy {
        service: String,
        field: &'static str,
        detail: &'static str,
    },
    #[error(
        "RTMP application `{application}` in service `{service}` has invalid `{field}`: {detail}"
    )]
    InvalidRtmpApplicationPolicy {
        service: String,
        application: String,
        field: &'static str,
        detail: &'static str,
    },
    #[error(
        "RTMP application `{application}` in service `{service}` requests DASH output, but no supported DASH muxer is available"
    )]
    UnsupportedRtmpDash {
        service: String,
        application: String,
    },
    #[error(
        "RTMP application `{application}` in service `{service}` has duplicate {operation} ACL rule `{network}`"
    )]
    DuplicateRtmpAccessRule {
        service: String,
        application: String,
        operation: &'static str,
        network: String,
    },
    #[error(
        "RTMP recorder `{recorder}` max_queue_bytes must not exceed max_storage_bytes in application `{application}` of service `{service}`"
    )]
    RtmpRecorderQueueExceedsStorage {
        service: String,
        application: String,
        recorder: String,
    },
    #[error(
        "RTMP recorders `{first_recorder}` and `{second_recorder}` use shared recording root `{root_directory}` and must use identical storage limits"
    )]
    RtmpRecorderStorageLimitsMismatch {
        root_directory: String,
        first_recorder: String,
        second_recorder: String,
    },
    #[error(
        "HTTP service `{service}` routes {first_route} and {duplicate_route} have equivalent matchers"
    )]
    DuplicateHttpRoute {
        service: String,
        first_route: usize,
        duplicate_route: usize,
    },
    #[error("HTTP service `{service}` route {route} references unknown upstream pool `{pool}`")]
    UnknownRouteUpstreamPool {
        service: String,
        route: usize,
        pool: String,
    },
    #[error("cache store `{store}` has invalid `{field}`: {detail}")]
    InvalidCacheStore {
        store: String,
        field: &'static str,
        detail: String,
    },
    #[error("HTTP service `{service}` route {route} references unknown cache store `{store}`")]
    UnknownCacheStore {
        service: String,
        route: usize,
        store: String,
    },
    #[error("HTTP service `{service}` route {route} has invalid cache `{field}`: {detail}")]
    InvalidCachePolicy {
        service: String,
        route: usize,
        field: &'static str,
        detail: String,
    },
    #[error("forward proxy service `{service}` has invalid `{field}`: {detail}")]
    InvalidForwardProxyService {
        service: String,
        field: &'static str,
        detail: String,
    },
    #[error("forward proxy listener `{listener}` has invalid configuration: {detail}")]
    InvalidForwardProxyListener { listener: String, detail: String },
    #[error("L4 service `{service}` references unknown upstream pool `{pool}`")]
    UnknownL4UpstreamPool { service: String, pool: String },
    #[error("L4 service `{service}` references TLS-enabled upstream pool `{pool}`")]
    TlsUpstreamPoolForL4Service { service: String, pool: String },
    #[error("L4 service `{service}` has invalid UDP policy `{field}`: {detail}")]
    InvalidL4UdpPolicy {
        service: String,
        field: &'static str,
        detail: &'static str,
    },
    #[error("{kind} `{name}` has invalid PROXY protocol `{field}`: {detail}")]
    InvalidProxyProtocolPolicy {
        kind: &'static str,
        name: String,
        field: &'static str,
        detail: &'static str,
    },
    #[error("management listener must use loopback, got `{0}`")]
    ManagementMustUseLoopback(SocketAddr),
    #[error(
        "statistics must configure between one and eight total unique IPv4/IPv6 listener binds"
    )]
    InvalidStatsBinds,
    #[error("statistics page {page} has invalid `{field}`: {detail}")]
    InvalidStatsPage {
        page: usize,
        field: &'static str,
        detail: &'static str,
    },
}
