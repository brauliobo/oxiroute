use std::time::{Duration, SystemTime};

use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};

use crate::{MonoTime, http::trim_ows, key::Vary};

/// Canonical freshness and retention windows applied by the server runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheTimeline {
    use_origin_cache_control: bool,
    default_ttl: Duration,
    status_ttls: Vec<(StatusCode, Duration)>,
    grace: Duration,
    keep: Duration,
}

impl CacheTimeline {
    /// Creates a deterministic canonical timeline.
    ///
    /// # Errors
    ///
    /// Returns an error when grace exceeds keep, a status is duplicated, or an informational,
    /// partial, or not-modified response is assigned a TTL.
    pub fn new(
        use_origin_cache_control: bool,
        default_ttl: Duration,
        status_ttls: impl IntoIterator<Item = (StatusCode, Duration)>,
        grace: Duration,
        keep: Duration,
    ) -> Result<Self, CacheTimelineError> {
        if grace > keep {
            return Err(CacheTimelineError::GraceExceedsKeep);
        }
        let mut status_ttls = status_ttls.into_iter().collect::<Vec<_>>();
        status_ttls.sort_unstable_by_key(|(status, _)| status.as_u16());
        for pair in status_ttls.windows(2) {
            if pair[0].0 == pair[1].0 {
                return Err(CacheTimelineError::DuplicateStatus);
            }
        }
        if status_ttls.iter().any(|(status, _)| {
            status.is_informational()
                || *status == StatusCode::PARTIAL_CONTENT
                || *status == StatusCode::NOT_MODIFIED
        }) {
            return Err(CacheTimelineError::UnsupportedStatus);
        }
        Ok(Self {
            use_origin_cache_control,
            default_ttl,
            status_ttls,
            grace,
            keep,
        })
    }

    fn status_ttl(&self, status: StatusCode) -> Option<Duration> {
        self.status_ttls
            .binary_search_by_key(&status.as_u16(), |(candidate, _)| candidate.as_u16())
            .ok()
            .map(|index| self.status_ttls[index].1)
    }
}

/// Invalid canonical cache timeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CacheTimelineError {
    #[error("cache grace must not exceed keep")]
    GraceExceedsKeep,
    #[error("cache status TTLs must be unique")]
    DuplicateStatus,
    #[error("cache status TTL cannot target informational, partial, or not-modified responses")]
    UnsupportedStatus,
}

/// Strictly parsed Cache-Control directives used by a shared cache.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CacheControl {
    pub no_store: bool,
    pub private: bool,
    pub no_cache: bool,
    pub must_revalidate: bool,
    pub proxy_revalidate: bool,
    pub public: bool,
    pub only_if_cached: bool,
    pub immutable: bool,
    pub max_age: Option<Duration>,
    pub shared_max_age: Option<Duration>,
    pub min_fresh: Option<Duration>,
    pub max_stale: Option<Option<Duration>>,
    pub stale_while_revalidate: Option<Duration>,
    pub stale_if_error: Option<Duration>,
}

impl CacheControl {
    /// Parses all Cache-Control field lines. Duplicate recognized directives and malformed values
    /// are rejected rather than interpreted ambiguously.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid directive grammar, duplicates, and overflowing delta-seconds.
    pub fn parse(headers: &HeaderMap) -> Result<Self, ParseError> {
        let mut parsed = Self::default();
        let mut seen = Vec::<Vec<u8>>::new();
        for value in headers.get_all(http::header::CACHE_CONTROL) {
            for item in comma_items(value.as_bytes())? {
                let (name, value) = directive(item)?;
                let lower = name.iter().map(u8::to_ascii_lowercase).collect::<Vec<_>>();
                let recognized = matches!(
                    lower.as_slice(),
                    b"no-store"
                        | b"private"
                        | b"no-cache"
                        | b"must-revalidate"
                        | b"proxy-revalidate"
                        | b"public"
                        | b"only-if-cached"
                        | b"immutable"
                        | b"max-age"
                        | b"s-maxage"
                        | b"min-fresh"
                        | b"max-stale"
                        | b"stale-while-revalidate"
                        | b"stale-if-error"
                );
                if recognized && seen.contains(&lower) {
                    return Err(ParseError::DuplicateDirective);
                }
                if recognized {
                    seen.push(lower.clone());
                }

                match lower.as_slice() {
                    b"no-store" => parsed.no_store = flag(value)?,
                    b"private" => parsed.private = optional_field_list(value)?,
                    b"no-cache" => parsed.no_cache = optional_field_list(value)?,
                    b"must-revalidate" => parsed.must_revalidate = flag(value)?,
                    b"proxy-revalidate" => parsed.proxy_revalidate = flag(value)?,
                    b"public" => parsed.public = flag(value)?,
                    b"only-if-cached" => parsed.only_if_cached = flag(value)?,
                    b"immutable" => parsed.immutable = flag(value)?,
                    b"max-age" => parsed.max_age = Some(delta_seconds(required(value)?)?),
                    b"s-maxage" => {
                        parsed.shared_max_age = Some(delta_seconds(required(value)?)?);
                    }
                    b"min-fresh" => parsed.min_fresh = Some(delta_seconds(required(value)?)?),
                    b"max-stale" => {
                        parsed.max_stale = Some(value.map(delta_seconds).transpose()?);
                    }
                    b"stale-while-revalidate" => {
                        parsed.stale_while_revalidate = Some(delta_seconds(required(value)?)?);
                    }
                    b"stale-if-error" => {
                        parsed.stale_if_error = Some(delta_seconds(required(value)?)?);
                    }
                    _ => {}
                }
            }
        }
        Ok(parsed)
    }
}

/// Request-side cache behavior after strict directive parsing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestMode {
    Lookup,
    Revalidate,
    Bypass,
}

/// Parsed request cache policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestPolicy {
    pub mode: RequestMode,
    pub only_if_cached: bool,
    pub max_age: Option<Duration>,
    pub min_fresh: Duration,
    pub max_stale: Option<Option<Duration>>,
}

impl RequestPolicy {
    /// Evaluates GET/HEAD request eligibility and request cache directives.
    ///
    /// # Errors
    ///
    /// Returns an error when Cache-Control is malformed.
    pub fn evaluate(method: &Method, headers: &HeaderMap) -> Result<Self, ParseError> {
        let control = CacheControl::parse(headers)?;
        let cache_method = *method == Method::GET || *method == Method::HEAD;
        let has_range = headers.contains_key(http::header::RANGE);
        let has_body = headers.contains_key(http::header::TRANSFER_ENCODING)
            || headers
                .get_all(http::header::CONTENT_LENGTH)
                .iter()
                .any(|value| value.as_bytes() != b"0");
        let has_condition = [
            http::header::IF_MATCH,
            http::header::IF_NONE_MATCH,
            http::header::IF_MODIFIED_SINCE,
            http::header::IF_UNMODIFIED_SINCE,
            http::header::IF_RANGE,
        ]
        .iter()
        .any(|name| headers.contains_key(name));
        let pragma_no_cache = !headers.contains_key(http::header::CACHE_CONTROL)
            && headers
                .get_all(http::header::PRAGMA)
                .iter()
                .any(|value| ascii_list_contains(value.as_bytes(), b"no-cache"));
        let mode = if !cache_method || has_range || has_body || has_condition || control.no_store {
            RequestMode::Bypass
        } else if control.no_cache || control.max_age == Some(Duration::ZERO) || pragma_no_cache {
            RequestMode::Revalidate
        } else {
            RequestMode::Lookup
        };
        Ok(Self {
            mode,
            only_if_cached: control.only_if_cached,
            max_age: control.max_age,
            min_fresh: control.min_fresh.unwrap_or_default(),
            max_stale: control.max_stale,
        })
    }
}

/// Monotonic and wall-clock timestamps captured around one upstream response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResponseTiming {
    pub request_started: MonoTime,
    pub response_received: MonoTime,
    pub response_received_wall: SystemTime,
}

/// RFC 9111 current-age calculation with saturating arithmetic.
#[must_use]
pub fn current_age(
    date: Option<SystemTime>,
    age_value: Duration,
    timing: ResponseTiming,
    now: MonoTime,
) -> Duration {
    let apparent_age = date.map_or(Duration::ZERO, |date| {
        timing
            .response_received_wall
            .duration_since(date)
            .unwrap_or_default()
    });
    let response_delay = timing
        .response_received
        .saturating_duration_since(timing.request_started);
    let corrected_age = age_value.saturating_add(response_delay);
    let corrected_initial_age = apparent_age.max(corrected_age);
    corrected_initial_age.saturating_add(now.saturating_duration_since(timing.response_received))
}

/// Validators retained for conditional upstream requests.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Validators {
    pub etag: Option<HeaderValue>,
    pub last_modified: Option<HeaderValue>,
}

impl Validators {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.etag.is_none() && self.last_modified.is_none()
    }

    /// Adds cache validators unless the outgoing request already has that condition.
    pub fn apply(&self, headers: &mut HeaderMap) {
        if let Some(etag) = &self.etag {
            headers
                .entry(http::header::IF_NONE_MATCH)
                .or_insert_with(|| etag.clone());
        }
        if let Some(last_modified) = &self.last_modified {
            headers
                .entry(http::header::IF_MODIFIED_SINCE)
                .or_insert_with(|| last_modified.clone());
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RetentionPolicy {
    Rfc,
    Canonical { keep: Duration },
}

impl RetentionPolicy {
    pub(crate) const fn request_stale_allowed(self) -> bool {
        matches!(self, Self::Rfc)
    }

    pub(crate) const fn keep(self) -> Option<Duration> {
        match self {
            Self::Rfc => None,
            Self::Canonical { keep } => Some(keep),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ResponsePolicy {
    pub freshness_lifetime: Duration,
    pub corrected_initial_age: Duration,
    pub stale_while_revalidate: Duration,
    pub stale_if_error: Duration,
    pub retention: RetentionPolicy,
    pub always_revalidate: bool,
    pub must_revalidate_stale: bool,
    pub allows_authorized_reuse: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct ResponsePolicyInput<'a> {
    pub request_headers: &'a HeaderMap,
    pub status: StatusCode,
    pub headers: &'a HeaderMap,
    pub timing: ResponseTiming,
    pub max_heuristic_freshness: Duration,
    pub max_vary_fields: usize,
    pub max_vary_bytes: usize,
    pub timeline: Option<&'a CacheTimeline>,
}

impl ResponsePolicy {
    pub fn evaluate(
        input: ResponsePolicyInput<'_>,
    ) -> Result<(Self, Vary, Validators), ResponseRejection> {
        let ResponsePolicyInput {
            request_headers,
            status,
            headers,
            timing,
            max_heuristic_freshness,
            max_vary_fields,
            max_vary_bytes,
            timeline,
        } = input;
        let control = CacheControl::parse(headers).map_err(ResponseRejection::InvalidMetadata)?;
        if status == StatusCode::PARTIAL_CONTENT || status == StatusCode::NOT_MODIFIED {
            return Err(ResponseRejection::Status);
        }
        if control.no_store {
            return Err(ResponseRejection::NoStore);
        }
        if control.private {
            return Err(ResponseRejection::Private);
        }
        if headers.contains_key(http::header::SET_COOKIE) {
            return Err(ResponseRejection::SetCookie);
        }
        if headers.keys().any(prohibited_stored_header) {
            return Err(ResponseRejection::HopByHop);
        }
        let allows_authorized_reuse =
            control.public || control.shared_max_age.is_some() || control.must_revalidate;
        if request_headers.contains_key(http::header::AUTHORIZATION) && !allows_authorized_reuse {
            return Err(ResponseRejection::Authorization);
        }
        let vary = Vary::parse(headers, max_vary_fields, max_vary_bytes)
            .map_err(|_| ResponseRejection::InvalidVary)?;
        if vary == Vary::Any {
            return Err(ResponseRejection::VaryAny);
        }

        let freshness = response_freshness(
            status,
            headers,
            timing,
            max_heuristic_freshness,
            timeline,
            &control,
        )?;
        let must_revalidate_stale = control.no_cache
            || control.must_revalidate
            || control.proxy_revalidate
            || control.shared_max_age.is_some();
        let validators = response_validators(headers)?;
        Ok((
            Self {
                freshness_lifetime: freshness.lifetime,
                corrected_initial_age: freshness.corrected_initial_age,
                stale_while_revalidate: freshness.stale_while_revalidate,
                stale_if_error: freshness.stale_if_error,
                retention: freshness.retention,
                always_revalidate: control.no_cache,
                must_revalidate_stale,
                allows_authorized_reuse,
            },
            vary,
            validators,
        ))
    }
}

struct ResponseFreshness {
    lifetime: Duration,
    corrected_initial_age: Duration,
    stale_while_revalidate: Duration,
    stale_if_error: Duration,
    retention: RetentionPolicy,
}

fn response_freshness(
    status: StatusCode,
    headers: &HeaderMap,
    timing: ResponseTiming,
    max_heuristic_freshness: Duration,
    timeline: Option<&CacheTimeline>,
    control: &CacheControl,
) -> Result<ResponseFreshness, ResponseRejection> {
    let date = optional_date(headers, http::header::DATE)?;
    let expires = optional_date(headers, http::header::EXPIRES)?;
    let last_modified = optional_date(headers, http::header::LAST_MODIFIED)?;
    let age = optional_delta_header(headers, http::header::AGE)?.unwrap_or_default();
    let explicit = control.shared_max_age.or(control.max_age).or_else(|| {
        expires.map(|expires| {
            expires
                .duration_since(date.unwrap_or(timing.response_received_wall))
                .unwrap_or_default()
        })
    });
    let configured_status_ttl = timeline.and_then(|timeline| timeline.status_ttl(status));
    if configured_status_ttl.is_none()
        && explicit.is_none()
        && !default_cacheable(status)
        && !control.public
    {
        return Err(ResponseRejection::Status);
    }
    let heuristic = last_modified.map_or(Duration::ZERO, |modified| {
        date.unwrap_or(timing.response_received_wall)
            .duration_since(modified)
            .unwrap_or_default()
            .div_f32(10.0)
            .min(max_heuristic_freshness)
    });
    let lifetime = timeline.map_or_else(
        || explicit.unwrap_or(heuristic),
        |timeline| {
            configured_status_ttl
                .or(if timeline.use_origin_cache_control {
                    explicit
                } else {
                    None
                })
                .unwrap_or(timeline.default_ttl)
        },
    );
    Ok(ResponseFreshness {
        lifetime,
        corrected_initial_age: current_age(date, age, timing, timing.response_received),
        stale_while_revalidate: timeline.map_or_else(
            || control.stale_while_revalidate.unwrap_or_default(),
            |_| Duration::ZERO,
        ),
        stale_if_error: timeline.map_or_else(
            || control.stale_if_error.unwrap_or_default(),
            |timeline| timeline.grace,
        ),
        retention: timeline.map_or(RetentionPolicy::Rfc, |timeline| {
            RetentionPolicy::Canonical {
                keep: timeline.keep,
            }
        }),
    })
}

fn response_validators(headers: &HeaderMap) -> Result<Validators, ResponseRejection> {
    let etag =
        single_header(headers, http::header::ETAG).map_err(ResponseRejection::InvalidMetadata)?;
    if etag.is_some_and(|value| !valid_etag(value.as_bytes())) {
        return Err(ResponseRejection::InvalidMetadata(
            ParseError::InvalidValidator,
        ));
    }
    Ok(Validators {
        etag: etag.cloned(),
        last_modified: single_header(headers, http::header::LAST_MODIFIED)
            .map_err(ResponseRejection::InvalidMetadata)?
            .cloned(),
    })
}

/// Safe failures that prevent a response from entering the shared cache.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ResponseRejection {
    #[error("response cache metadata is malformed")]
    InvalidMetadata(#[source] ParseError),
    #[error("response has Cache-Control: no-store")]
    NoStore,
    #[error("response is private")]
    Private,
    #[error("response contains Set-Cookie")]
    SetCookie,
    #[error("response contains hop-by-hop metadata")]
    HopByHop,
    #[error("authenticated response lacks explicit shared-cache permission")]
    Authorization,
    #[error("response status is not cacheable")]
    Status,
    #[error("response Vary metadata is malformed or exceeds bounds")]
    InvalidVary,
    #[error("Vary: * responses are not reusable")]
    VaryAny,
    #[error("response timing is not monotonic")]
    InvalidTiming,
}

/// Cache metadata parse failure. Wire bytes are treated as data and never converted with `unwrap`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ParseError {
    #[error("invalid Cache-Control directive syntax")]
    InvalidDirective,
    #[error("duplicate Cache-Control directive")]
    DuplicateDirective,
    #[error("invalid or overflowing delta-seconds")]
    InvalidDeltaSeconds,
    #[error("duplicate singleton cache metadata field")]
    DuplicateField,
    #[error("invalid HTTP date")]
    InvalidDate,
    #[error("invalid entity tag validator")]
    InvalidValidator,
}

/// A 304 response cannot safely update the stored representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RevalidationError {
    #[error("304 metadata contains a prohibited field")]
    ProhibitedField,
}

/// Merges end-to-end 304 metadata into stored response headers.
///
/// Hop-by-hop fields, `Content-Length`, and `Set-Cookie` are rejected. The caller must separately
/// verify that the resulting `Vary` key still identifies the same representation.
///
/// # Errors
///
/// Returns an error if prohibited metadata is present.
pub fn merge_not_modified_headers(
    stored: &HeaderMap,
    not_modified: &HeaderMap,
) -> Result<HeaderMap, RevalidationError> {
    let mut merged = stored.clone();
    let mut names = Vec::<HeaderName>::new();
    for name in not_modified.keys() {
        if prohibited_304(name) {
            return Err(RevalidationError::ProhibitedField);
        }
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    for name in names {
        merged.remove(&name);
        for value in not_modified.get_all(&name) {
            merged.append(name.clone(), value.clone());
        }
    }
    Ok(merged)
}

fn prohibited_304(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "content-length"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "set-cookie"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn prohibited_stored_header(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn default_cacheable(status: StatusCode) -> bool {
    matches!(
        status.as_u16(),
        200 | 203 | 204 | 300 | 301 | 308 | 404 | 405 | 410 | 414 | 501
    )
}

fn valid_etag(value: &[u8]) -> bool {
    let value = value.strip_prefix(b"W/").unwrap_or(value);
    value.len() >= 2
        && value.first() == Some(&b'"')
        && value.last() == Some(&b'"')
        && value[1..value.len() - 1]
            .iter()
            .all(|byte| *byte == 0x21 || (0x23..=0x7e).contains(byte) || *byte >= 0x80)
}

fn optional_date(
    headers: &HeaderMap,
    name: HeaderName,
) -> Result<Option<SystemTime>, ResponseRejection> {
    single_header(headers, name)
        .map_err(ResponseRejection::InvalidMetadata)?
        .map(|value| {
            httpdate::parse_http_date(
                std::str::from_utf8(value.as_bytes())
                    .map_err(|_| ResponseRejection::InvalidMetadata(ParseError::InvalidDate))?,
            )
            .map_err(|_| ResponseRejection::InvalidMetadata(ParseError::InvalidDate))
        })
        .transpose()
}

fn optional_delta_header(
    headers: &HeaderMap,
    name: HeaderName,
) -> Result<Option<Duration>, ResponseRejection> {
    single_header(headers, name)
        .map_err(ResponseRejection::InvalidMetadata)?
        .map(|value| delta_seconds(value.as_bytes()).map_err(ResponseRejection::InvalidMetadata))
        .transpose()
}

fn single_header(
    headers: &HeaderMap,
    name: HeaderName,
) -> Result<Option<&HeaderValue>, ParseError> {
    let mut values = headers.get_all(name).iter();
    let first = values.next();
    if values.next().is_some() {
        return Err(ParseError::DuplicateField);
    }
    Ok(first)
}

fn comma_items(value: &[u8]) -> Result<Vec<&[u8]>, ParseError> {
    let mut items = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (index, byte) in value.iter().copied().enumerate() {
        if escaped {
            escaped = false;
        } else if quoted && byte == b'\\' {
            escaped = true;
        } else if byte == b'"' {
            quoted = !quoted;
        } else if byte == b',' && !quoted {
            let item = trim_ows(&value[start..index]);
            if item.is_empty() {
                return Err(ParseError::InvalidDirective);
            }
            items.push(item);
            start = index + 1;
        }
    }
    if quoted || escaped {
        return Err(ParseError::InvalidDirective);
    }
    let final_item = trim_ows(&value[start..]);
    if final_item.is_empty() {
        return Err(ParseError::InvalidDirective);
    }
    items.push(final_item);
    Ok(items)
}

fn directive(item: &[u8]) -> Result<(&[u8], Option<&[u8]>), ParseError> {
    let (name, value) = item
        .iter()
        .position(|byte| *byte == b'=')
        .map_or((item, None), |index| {
            (trim_ows(&item[..index]), Some(trim_ows(&item[index + 1..])))
        });
    if name.is_empty() || !name.iter().copied().all(is_token) {
        return Err(ParseError::InvalidDirective);
    }
    if let Some(value) = value
        && (value.is_empty() || !valid_directive_value(value))
    {
        return Err(ParseError::InvalidDirective);
    }
    Ok((name, value))
}

fn valid_directive_value(value: &[u8]) -> bool {
    if value.iter().copied().all(is_token) {
        return true;
    }
    if value.len() < 2 || value.first() != Some(&b'"') || value.last() != Some(&b'"') {
        return false;
    }
    let mut escaped = false;
    for byte in &value[1..value.len() - 1] {
        if escaped {
            if !(*byte == b'\t' || (*byte >= b' ' && *byte != 0x7f)) {
                return false;
            }
            escaped = false;
        } else if *byte == b'\\' {
            escaped = true;
        } else if *byte == b'"' || !(*byte == b'\t' || (*byte >= b' ' && *byte != 0x7f)) {
            return false;
        }
    }
    !escaped
}

fn flag(value: Option<&[u8]>) -> Result<bool, ParseError> {
    if value.is_some() {
        Err(ParseError::InvalidDirective)
    } else {
        Ok(true)
    }
}

fn optional_field_list(value: Option<&[u8]>) -> Result<bool, ParseError> {
    if value.is_some_and(|value| value.first() != Some(&b'"')) {
        return Err(ParseError::InvalidDirective);
    }
    Ok(true)
}

fn required(value: Option<&[u8]>) -> Result<&[u8], ParseError> {
    value.ok_or(ParseError::InvalidDirective)
}

fn delta_seconds(value: &[u8]) -> Result<Duration, ParseError> {
    if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
        return Err(ParseError::InvalidDeltaSeconds);
    }
    let mut seconds = 0u64;
    for digit in value {
        seconds = seconds
            .checked_mul(10)
            .and_then(|number| number.checked_add(u64::from(digit - b'0')))
            .ok_or(ParseError::InvalidDeltaSeconds)?;
    }
    Ok(Duration::from_secs(seconds))
}

fn ascii_list_contains(value: &[u8], expected: &[u8]) -> bool {
    value
        .split(|byte| *byte == b',')
        .map(trim_ows)
        .any(|item| item.eq_ignore_ascii_case(expected))
}

const fn is_token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}
