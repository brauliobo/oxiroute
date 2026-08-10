#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpService {
    pub name: String,
    pub routes: Vec<HttpRoute>,
    #[serde(default = "default_true")]
    pub automatic_response_headers: bool,
    #[serde(default = "default_upstream_io_timeout_ms")]
    pub upstream_io_timeout_ms: u64,
    /// Request body cap. Omitted configs default to 10 MiB; explicit null means unbounded.
    #[serde(default = "default_max_request_body_bytes")]
    pub max_request_body_bytes: Option<u64>,
    #[serde(default)]
    pub gzip: Option<HttpGzipPolicy>,
    #[serde(default)]
    pub access_log: Option<AccessLogPolicy>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpRoute {
    /// Host precedence is exact authority, normalized exact/IP, normalized wildcard, then none.
    #[serde(default)]
    pub host: Option<HttpHostSelector>,
    /// Path precedence is exact, segment prefix, then raw prefix; longer prefixes win within kind.
    pub path: HttpPathSelector,
    /// A nonempty method set precedes an any-method route; source order resolves final ties.
    #[serde(default)]
    pub methods: Vec<String>,
    #[serde(default)]
    pub access_policy: Option<HttpAccessPolicy>,
    #[serde(default = "default_http_route_policy")]
    pub policy: HttpRoutePolicy,
    pub action: HttpRouteAction,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpRoutePolicy {
    #[serde(default = "default_max_request_body_bytes")]
    pub max_request_body_bytes: Option<u64>,
    #[serde(default = "default_upstream_io_timeout_ms")]
    pub connect_timeout_ms: u64,
    #[serde(default = "default_upstream_io_timeout_ms")]
    pub read_timeout_ms: u64,
    #[serde(default = "default_upstream_io_timeout_ms")]
    pub write_timeout_ms: u64,
    #[serde(default)]
    pub request_buffering: bool,
    #[serde(default)]
    pub response_buffering: bool,
}

impl HttpRoutePolicy {
    pub(crate) const fn new() -> Self {
        Self {
            max_request_body_bytes: Some(10 * 1024 * 1024),
            connect_timeout_ms: 30_000,
            read_timeout_ms: 30_000,
            write_timeout_ms: 30_000,
            request_buffering: false,
            response_buffering: false,
        }
    }
}

impl Default for HttpRoutePolicy {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpGzipPolicy {
    pub level: u8,
    pub content_types: Vec<String>,
    #[serde(default = "default_http_gzip_min_length_bytes")]
    pub min_length_bytes: u64,
    #[serde(default)]
    pub min_http_version: HttpGzipMinimumVersion,
    #[serde(default)]
    pub disable_on_via: bool,
    #[serde(default = "default_true")]
    pub vary: bool,
}

impl Default for HttpGzipPolicy {
    fn default() -> Self {
        Self {
            level: 1,
            content_types: vec!["text/html".into()],
            min_length_bytes: default_http_gzip_min_length_bytes(),
            min_http_version: HttpGzipMinimumVersion::default(),
            disable_on_via: false,
            vary: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum HttpGzipMinimumVersion {
    #[default]
    #[serde(rename = "1.0")]
    Http10,
    #[serde(rename = "1.1")]
    Http11,
}

const fn default_http_gzip_min_length_bytes() -> u64 {
    20
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AccessLogPolicy {
    Disabled,
    File { path: PathBuf },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HttpHostSelector {
    NormalizedHost {
        value: String,
    },
    ExactAuthority {
        value: String,
    },
    AsciiCaseInsensitiveExactAuthority {
        value: String,
    },
    /// nginx `*.example.com`: matches one or more labels before the suffix.
    NginxLeadingWildcard {
        value: String,
    },
    /// nginx `.example.com`: matches the suffix itself and any leading labels.
    NginxLeadingDot {
        value: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HttpPathSelector {
    SegmentPrefix { value: String },
    RawPrefix { value: String },
    Exact { value: String },
    AsciiCaseInsensitiveExact { value: String },
}

impl HttpPathSelector {
    pub(crate) fn value_mut(&mut self) -> &mut String {
        match self {
            Self::SegmentPrefix { value }
            | Self::RawPrefix { value }
            | Self::Exact { value }
            | Self::AsciiCaseInsensitiveExact { value } => value,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HttpAccessPolicy {
    BearerTokenFile {
        token_file_path: PathBuf,
        #[serde(default = "default_http_access_header_name")]
        header_name: String,
        #[serde(default)]
        realm: Option<String>,
    },
    BasicHtpasswdFile {
        htpasswd_file_path: PathBuf,
        realm: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HttpRouteAction {
    Proxy {
        upstream_pool: String,
        policy: HttpProxyPolicy,
    },
    FixedResponse {
        status: u16,
        #[serde(default)]
        body: String,
        #[serde(default)]
        headers: Vec<HttpLiteralHeader>,
    },
    Redirect {
        #[serde(default = "default_http_redirect_status")]
        status: u16,
        location: HttpRedirectLocation,
        #[serde(default)]
        headers: Vec<HttpLiteralHeader>,
    },
    StaticFiles {
        root_directory: PathBuf,
        #[serde(default)]
        path_mapping: HttpStaticPathMapping,
        #[serde(default = "default_http_static_index_files")]
        index_files: Vec<String>,
        #[serde(default)]
        internal_index_redirects: bool,
        #[serde(default)]
        directory_redirects: bool,
        #[serde(default)]
        spa_fallback: Option<PathBuf>,
        #[serde(default)]
        try_files: Vec<HttpStaticTryFile>,
        #[serde(default)]
        autoindex: bool,
        #[serde(default = "default_true")]
        autoindex_exact_size: bool,
        #[serde(default)]
        autoindex_local_time: bool,
        #[serde(default = "default_true", deserialize_with = "deserialize_strict_bool")]
        etag: bool,
        #[serde(default)]
        mime: HttpStaticMimePolicy,
        #[serde(default)]
        headers: Vec<HttpLiteralHeader>,
        #[serde(default)]
        error_responses: Vec<HttpStaticErrorResponse>,
    },
}

fn deserialize_strict_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    struct StrictBoolVisitor;

    impl serde::de::Visitor<'_> for StrictBoolVisitor {
        type Value = bool;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a boolean for etag")
        }

        fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
            Ok(value)
        }
    }

    deserializer.deserialize_any(StrictBoolVisitor)
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HttpStaticPathMapping {
    #[default]
    Root,
    Alias,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HttpStaticTryFile {
    RequestPath,
    RequestPathDirectory,
    Relative { path: PathBuf },
    Status { status: u16 },
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpStaticMimePolicy {
    #[serde(default)]
    pub default_type: Option<String>,
    #[serde(default)]
    pub types: Vec<HttpMimeType>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpMimeType {
    pub extension: String,
    pub content_type: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpStaticErrorResponse {
    pub statuses: Vec<u16>,
    #[serde(default)]
    pub file: Option<PathBuf>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub headers: Vec<HttpLiteralHeader>,
    #[serde(default)]
    pub internal_redirect: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpProxyPolicy {
    #[serde(default)]
    pub upstream_host: HttpUpstreamHost,
    #[serde(default)]
    pub upstream_path_rewrite: Option<HttpProxyPathRewrite>,
    #[serde(default)]
    pub request_headers: Vec<HttpRequestHeaderMutation>,
    #[serde(default)]
    pub response_headers: Vec<HttpResponseHeaderMutation>,
    #[serde(default)]
    pub response_cookie_path_rewrites: Vec<HttpCookiePathRewrite>,
    #[serde(default)]
    pub response_cookie_attributes: Vec<HttpCookieAttributePolicy>,
    #[serde(default)]
    pub retry: HttpRetryPolicy,
    #[serde(default)]
    pub cache: Option<Box<HttpCachePolicy>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpCachePolicy {
    pub store: String,
    #[serde(default = "default_cache_methods")]
    pub methods: Vec<String>,
    #[serde(default = "default_cache_key_components")]
    pub key_components: Vec<CacheKeyComponent>,
    #[serde(default = "default_true")]
    pub use_origin_cache_control: bool,
    #[serde(default = "default_cache_ttl_ms")]
    pub default_ttl_ms: u64,
    #[serde(default)]
    pub status_ttls: Vec<CacheStatusTtl>,
    #[serde(default = "default_cache_grace_ms")]
    pub grace_ms: u64,
    #[serde(default = "default_cache_keep_ms")]
    pub keep_ms: u64,
    #[serde(default = "default_true")]
    pub revalidate: bool,
    #[serde(default = "default_true")]
    pub collapsed_forwarding: bool,
    #[serde(default)]
    pub stale_on: Vec<CacheStaleTrigger>,
    #[serde(default)]
    pub bypass_request: Vec<CachePredicate>,
    #[serde(default)]
    pub no_store_request: Vec<CachePredicate>,
    #[serde(default)]
    pub no_store_response: Vec<CachePredicate>,
    #[serde(default)]
    pub set_cookie_policy: CacheSetCookiePolicy,
    #[serde(default)]
    pub authorization_policy: CacheAuthorizationPolicy,
    #[serde(default)]
    pub vary_policy: CacheVaryPolicy,
    #[serde(default)]
    pub surrogate_tags: Option<CacheSurrogateTags>,
    #[serde(default)]
    pub purge_authorization: Option<CachePurgeAuthorization>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CacheKeyComponent {
    Scheme,
    NormalizedHost,
    PathAndQuery,
    Header { name: String },
    Cookie { name: String },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CacheStatusTtl {
    pub status: u16,
    pub ttl_ms: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CacheStaleTrigger {
    ConnectFailure,
    ConnectTimeout,
    #[serde(rename = "origin_500")]
    Origin500,
    #[serde(rename = "origin_502")]
    Origin502,
    #[serde(rename = "origin_503")]
    Origin503,
    #[serde(rename = "origin_504")]
    Origin504,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CachePredicate {
    HeaderPresent { name: String },
    CookiePresent { name: String },
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheSetCookiePolicy {
    #[default]
    Bypass,
    Ignore,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheAuthorizationPolicy {
    #[default]
    Bypass,
    Cache,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheVaryPolicy {
    #[default]
    Respect,
    Ignore,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CacheSurrogateTags {
    pub response_header: String,
    #[serde(default = "default_cache_max_tags_per_object")]
    pub max_tags: u64,
    #[serde(default = "default_cache_max_tag_bytes")]
    pub max_tag_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CachePurgeAuthorization {
    BearerTokenFile { token_file_path: PathBuf },
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HttpUpstreamHost {
    #[default]
    PreserveIncoming,
    NginxHost {
        fallback: String,
    },
    Endpoint {
        #[serde(default)]
        unix_fallback: Option<String>,
    },
    Literal {
        value: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum HttpRequestHeaderMutation {
    Set {
        name: String,
        value: HttpRequestHeaderValue,
    },
    Remove {
        name: String,
    },
}

impl HttpRequestHeaderMutation {
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Set { name, .. } | Self::Remove { name } => name,
        }
    }

    pub(crate) fn name_mut(&mut self) -> &mut String {
        match self {
            Self::Set { name, .. } | Self::Remove { name } => name,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HttpRequestHeaderValue {
    Literal {
        value: String,
    },
    IncomingAuthority,
    NormalizedHost,
    NginxHost {
        fallback: String,
    },
    ClientIp,
    AppendedXForwardedFor {
        max_bytes: u64,
        #[serde(default)]
        except_source_cidrs: Vec<String>,
    },
    DownstreamScheme,
    IncomingHeader {
        name: String,
        max_bytes: u64,
    },
    SelectedUpstreamHost,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpCookieAttributePolicy {
    pub name: String,
    #[serde(default)]
    pub secure: Option<bool>,
    #[serde(default)]
    pub http_only: Option<bool>,
    #[serde(default)]
    pub same_site: Option<HttpSameSite>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HttpSameSite {
    Strict,
    Lax,
    None,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum HttpResponseHeaderMutation {
    Set {
        name: String,
        value: String,
        #[serde(default = "default_true")]
        always: bool,
    },
    Add {
        name: String,
        value: String,
        #[serde(default = "default_true")]
        always: bool,
    },
    Remove {
        name: String,
    },
}

impl HttpResponseHeaderMutation {
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Set { name, .. } | Self::Add { name, .. } | Self::Remove { name } => name,
        }
    }

    pub(crate) fn name_mut(&mut self) -> &mut String {
        match self {
            Self::Set { name, .. } | Self::Add { name, .. } | Self::Remove { name } => name,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpCookiePathRewrite {
    pub from: String,
    pub to: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpProxyPathRewrite {
    pub from: String,
    pub to: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpRetryPolicy {
    #[serde(default)]
    pub max_retries: u8,
    #[serde(
        default = "default_http_retry_triggers",
        deserialize_with = "deserialize_http_retry_triggers"
    )]
    pub triggers: Vec<HttpRetryTrigger>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub response_statuses: Vec<u16>,
    #[serde(default)]
    pub method_safety: HttpRetryMethodSafety,
    #[serde(default)]
    pub body_safety: HttpRetryBodySafety,
    #[serde(default)]
    pub target: HttpRetryTarget,
    #[serde(default)]
    pub delay_ms: u64,
    #[serde(default)]
    pub final_redispatch: bool,
}

fn deserialize_http_retry_triggers<'de, D>(
    deserializer: D,
) -> Result<Vec<HttpRetryTrigger>, D::Error>
where
    D: Deserializer<'de>,
{
    struct RetryTriggersVisitor;

    impl<'de> Visitor<'de> for RetryTriggersVisitor {
        type Value = Vec<HttpRetryTrigger>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a sequence of HTTP retry triggers")
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut triggers = Vec::new();
            while let Some(trigger) = sequence.next_element()? {
                triggers.push(trigger);
            }
            Ok(triggers)
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            if map
                .next_entry::<de::IgnoredAny, de::IgnoredAny>()?
                .is_some()
            {
                return Err(de::Error::custom("HTTP retry triggers must be a sequence"));
            }
            Ok(Vec::new())
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(Vec::new())
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(Vec::new())
        }
    }

    deserializer.deserialize_any(RetryTriggersVisitor)
}

impl Default for HttpRetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 0,
            triggers: default_http_retry_triggers(),
            response_statuses: Vec::new(),
            method_safety: HttpRetryMethodSafety::default(),
            body_safety: HttpRetryBodySafety::default(),
            target: HttpRetryTarget::default(),
            delay_ms: 0,
            final_redispatch: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HttpRetryTarget {
    SameServer,
    #[default]
    NextServer,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum HttpRetryTrigger {
    ConnectFailure,
    ConnectTimeout,
    RefusedStream,
    EmptyResponse,
    ResponseTimeout,
    JunkResponse,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HttpRetryMethodSafety {
    #[default]
    GetHead,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HttpRetryBodySafety {
    #[default]
    Empty,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpLiteralHeader {
    pub name: String,
    pub value: String,
    #[serde(default = "default_true")]
    pub always: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HttpRedirectLocation {
    Literal {
        value: String,
    },
    RequestTemplate {
        value: String,
        #[serde(default)]
        nginx_host_fallback: Option<String>,
    },
}
