#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwardProxyService {
    pub name: String,
    pub enabled_versions: Vec<ForwardHttpVersion>,
    pub allow_absolute_form: bool,
    pub tls_required: bool,
    pub connect: ForwardConnectPolicy,
    pub connect_udp: ForwardConnectPolicy,
    pub peer_policy: ForwardPeerPolicy,
    pub auth: Option<ForwardProxyAuth>,
    pub access_policy: Option<ForwardAccessPolicy>,
    pub destination_policy: ForwardDestinationPolicy,
    pub header_policy: ForwardHeaderPolicy,
    pub connect_timeout_ms: u64,
    pub idle_timeout_ms: u64,
    pub lifetime_timeout_ms: u64,
    pub max_request_body_bytes: Option<u64>,
    pub max_header_bytes: u64,
    pub max_connections: u64,
    pub resolver: ForwardResolverPolicy,
    pub audit_mode: ForwardAuditMode,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ForwardProxyServiceWire {
    name: String,
    #[serde(default = "default_forward_http_versions")]
    enabled_versions: Vec<ForwardHttpVersion>,
    #[serde(default = "default_true")]
    allow_absolute_form: bool,
    #[serde(default = "default_true")]
    tls_required: bool,
    #[serde(default)]
    connect: ForwardConnectPolicy,
    #[serde(default)]
    connect_udp: ForwardConnectPolicy,
    #[serde(default)]
    peer_policy: ForwardPeerPolicy,
    #[serde(default)]
    auth: Option<ForwardProxyAuth>,
    #[serde(default)]
    access_policy: Option<ForwardAccessPolicy>,
    #[serde(default)]
    destination_policy: ForwardDestinationPolicy,
    #[serde(default)]
    header_policy: ForwardHeaderPolicy,
    #[serde(default)]
    cache: Option<Box<HttpCachePolicy>>,
    #[serde(default = "default_connect_timeout_ms")]
    connect_timeout_ms: u64,
    #[serde(default = "default_idle_timeout_ms")]
    idle_timeout_ms: u64,
    #[serde(default = "default_forward_lifetime_timeout_ms")]
    lifetime_timeout_ms: u64,
    #[serde(default = "default_max_request_body_bytes")]
    max_request_body_bytes: Option<u64>,
    #[serde(default = "default_forward_max_header_bytes")]
    max_header_bytes: u64,
    #[serde(default = "default_forward_max_connections")]
    max_connections: u64,
    #[serde(default)]
    resolver: ForwardResolverPolicy,
    #[serde(default)]
    audit_mode: ForwardAuditMode,
}

impl<'de> Deserialize<'de> for ForwardProxyService {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ForwardProxyServiceWire::deserialize(deserializer)?;
        let mut header_policy = wire.header_policy;
        header_policy.cache = wire.cache;
        Ok(Self {
            name: wire.name,
            enabled_versions: wire.enabled_versions,
            allow_absolute_form: wire.allow_absolute_form,
            tls_required: wire.tls_required,
            connect: wire.connect,
            connect_udp: wire.connect_udp,
            peer_policy: wire.peer_policy,
            auth: wire.auth,
            access_policy: wire.access_policy,
            destination_policy: wire.destination_policy,
            header_policy,
            connect_timeout_ms: wire.connect_timeout_ms,
            idle_timeout_ms: wire.idle_timeout_ms,
            lifetime_timeout_ms: wire.lifetime_timeout_ms,
            max_request_body_bytes: wire.max_request_body_bytes,
            max_header_bytes: wire.max_header_bytes,
            max_connections: wire.max_connections,
            resolver: wire.resolver,
            audit_mode: wire.audit_mode,
        })
    }
}

impl Serialize for ForwardProxyService {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut header_policy = self.header_policy.clone();
        let cache = header_policy.cache.take();
        ForwardProxyServiceWire {
            name: self.name.clone(),
            enabled_versions: self.enabled_versions.clone(),
            allow_absolute_form: self.allow_absolute_form,
            tls_required: self.tls_required,
            connect: self.connect.clone(),
            connect_udp: self.connect_udp.clone(),
            peer_policy: self.peer_policy.clone(),
            auth: self.auth.clone(),
            access_policy: self.access_policy.clone(),
            destination_policy: self.destination_policy.clone(),
            header_policy,
            cache,
            connect_timeout_ms: self.connect_timeout_ms,
            idle_timeout_ms: self.idle_timeout_ms,
            lifetime_timeout_ms: self.lifetime_timeout_ms,
            max_request_body_bytes: self.max_request_body_bytes,
            max_header_bytes: self.max_header_bytes,
            max_connections: self.max_connections,
            resolver: self.resolver.clone(),
            audit_mode: self.audit_mode,
        }
        .serialize(serializer)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum ForwardHttpVersion {
    H1,
    H2,
    H3,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ForwardConnectPolicy {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_forward_connect_ports")]
    pub allowed_ports: Vec<u16>,
}

impl Default for ForwardConnectPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            allowed_ports: default_forward_connect_ports(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ForwardPeerPolicy {
    #[serde(default)]
    pub peers: Vec<ForwardPeer>,
    #[serde(default)]
    pub direct_fallback: ForwardDirectFallback,
    #[serde(default = "default_forward_peer_max_retries")]
    pub max_retries: u8,
}

impl Default for ForwardPeerPolicy {
    fn default() -> Self {
        Self {
            peers: Vec::new(),
            direct_fallback: ForwardDirectFallback::default(),
            max_retries: default_forward_peer_max_retries(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ForwardPeer {
    pub host: String,
    pub port: u16,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ForwardDirectFallback {
    #[default]
    Allowed,
    Denied,
    Required,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ForwardProxyAuth {
    BearerTokenFile {
        token_file_path: PathBuf,
    },
    BasicHtpasswdFile {
        htpasswd_file_path: PathBuf,
        realm: String,
        #[serde(default)]
        credential_ttl_ms: Option<u64>,
        #[serde(default = "default_true")]
        username_case_sensitive: bool,
    },
    /// Reserved for a future listener TLS client-certificate verifier integration.
    MutualTls {
        client_ca_file_path: PathBuf,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ForwardAccessPolicy {
    #[serde(default)]
    pub rules: Vec<ForwardAccessRule>,
    #[serde(default)]
    pub default_action: ForwardAccessAction,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ForwardAccessRule {
    pub action: ForwardAccessAction,
    #[serde(default)]
    pub conditions: Vec<ForwardAccessCondition>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ForwardAccessAction {
    Allow,
    #[default]
    Deny,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ForwardAccessCondition {
    #[serde(default)]
    pub negated: bool,
    #[serde(flatten)]
    pub matcher: ForwardAccessMatcher,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ForwardAccessMatcher {
    All,
    Methods { methods: Vec<String> },
    SourceCidrs { cidrs: Vec<String> },
    DestinationPorts { ranges: Vec<ForwardPortRange> },
    Authenticated,
    DestinationLocal,
    DestinationLinkLocal,
    Manager,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ForwardPortRange {
    pub start: u16,
    pub end: u16,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ForwardHeaderPolicy {
    #[serde(default)]
    pub forwarded_for: ForwardedForPolicy,
    #[serde(default)]
    pub via: ForwardViaPolicy,
    #[serde(skip)]
    pub cache: Option<Box<HttpCachePolicy>>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ForwardedForPolicy {
    #[default]
    Preserve,
    Delete,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ForwardViaPolicy {
    #[default]
    Preserve,
    Delete,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ForwardDestinationPolicy {
    #[serde(default)]
    pub allow_domains: Vec<String>,
    #[serde(default)]
    pub deny_domains: Vec<String>,
    #[serde(default)]
    pub allow_cidrs: Vec<String>,
    #[serde(default)]
    pub deny_cidrs: Vec<String>,
    #[serde(default = "default_true")]
    pub deny_private: bool,
    #[serde(default)]
    pub allow_times: Vec<ForwardTimeRange>,
    #[serde(default)]
    pub deny_times: Vec<ForwardTimeRange>,
}

impl Default for ForwardDestinationPolicy {
    fn default() -> Self {
        Self {
            allow_domains: Vec::new(),
            deny_domains: Vec::new(),
            allow_cidrs: Vec::new(),
            deny_cidrs: Vec::new(),
            deny_private: true,
            allow_times: Vec::new(),
            deny_times: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ForwardTimeRange {
    pub days: Vec<ForwardWeekday>,
    pub start: String,
    pub end: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum ForwardWeekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ForwardResolverPolicy {
    #[serde(default)]
    pub nameservers: Vec<IpAddr>,
    #[serde(default = "default_forward_resolver_cache_entries")]
    pub max_cache_entries: u64,
    #[serde(default = "default_forward_resolver_concurrent_queries")]
    pub max_concurrent_queries: u64,
    #[serde(default = "default_forward_resolver_max_addresses")]
    pub max_addresses_per_name: u64,
    #[serde(default = "default_forward_resolver_min_ttl_ms")]
    pub min_ttl_ms: u64,
    #[serde(default = "default_forward_resolver_max_ttl_ms")]
    pub max_ttl_ms: u64,
    #[serde(default = "default_forward_resolver_negative_ttl_ms")]
    pub negative_ttl_ms: u64,
    #[serde(default = "default_true")]
    pub revalidate_on_connect: bool,
}

impl Default for ForwardResolverPolicy {
    fn default() -> Self {
        Self {
            nameservers: Vec::new(),
            max_cache_entries: default_forward_resolver_cache_entries(),
            max_concurrent_queries: default_forward_resolver_concurrent_queries(),
            max_addresses_per_name: default_forward_resolver_max_addresses(),
            min_ttl_ms: default_forward_resolver_min_ttl_ms(),
            max_ttl_ms: default_forward_resolver_max_ttl_ms(),
            negative_ttl_ms: default_forward_resolver_negative_ttl_ms(),
            revalidate_on_connect: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ForwardAuditMode {
    Off,
    #[default]
    Metadata,
}
