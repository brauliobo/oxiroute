use std::time::SystemTime;

use http::{
    HeaderValue, Response, StatusCode,
    header::{
        ALLOW, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, ETAG, HeaderName, WWW_AUTHENTICATE,
    },
};
use serde::Serialize;
use serde_json::Value;

use super::dto::ErrorResponse;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApiResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub allow: Option<&'static str>,
    pub content_type: &'static str,
    pub www_authenticate: Option<&'static str>,
    pub correlation_id: Option<String>,
    pub content_range: Option<String>,
    pub accept_ranges: bool,
    pub etag: Option<String>,
    pub cache_control: Option<&'static str>,
}

impl ApiResponse {
    pub(crate) fn bytes(status: u16, body: Vec<u8>, content_type: &'static str) -> Self {
        Self {
            status,
            body,
            allow: None,
            content_type,
            www_authenticate: None,
            correlation_id: None,
            content_range: None,
            accept_ranges: false,
            etag: None,
            cache_control: None,
        }
    }

    /// Serializes a response DTO straight to bytes, without an intermediate `serde_json::Value`.
    pub(crate) fn dto(status: u16, response: &impl Serialize) -> Self {
        let body = serde_json::to_vec(response).expect("management response DTO serializes");
        Self::bytes(status, body, "application/json")
    }

    /// `304 Not Modified` for a validator the client already holds.
    pub(crate) fn not_modified(etag: String) -> Self {
        let mut response = Self::bytes(304, Vec::new(), "application/json");
        response.etag = Some(etag);
        response
    }

    pub(crate) fn with_cache_control(mut self, cache_control: &'static str) -> Self {
        self.cache_control = Some(cache_control);
        self
    }

    pub(crate) fn with_etag(mut self, etag: String) -> Self {
        self.etag = Some(etag);
        self
    }

    pub(crate) fn json(status: u16, value: &Value) -> Self {
        Self::bytes(status, value.to_string().into_bytes(), "application/json")
    }

    pub(crate) fn error(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        let value = serde_json::to_value(ErrorResponse::new(code, message.into()))
            .expect("error response DTO serializes");
        Self::json(status, &value)
    }

    pub(crate) fn route_not_found() -> Self {
        Self::error(404, "route_not_found", "route does not exist")
    }

    pub(crate) fn method_not_allowed(allow: &'static str) -> Self {
        let mut response = Self::error(405, "method_not_allowed", "method is not allowed");
        response.allow = Some(allow);
        response
    }

    pub(crate) fn unauthorized() -> Self {
        let mut response = Self::error(
            401,
            "unauthorized",
            "a valid Bearer management token is required",
        );
        response.www_authenticate = Some("Bearer");
        response
    }

    pub(crate) fn with_correlation(mut self, correlation_id: String) -> Self {
        self.correlation_id = Some(correlation_id);
        self
    }

    pub(crate) fn with_range(mut self, content_range: Option<String>) -> Self {
        self.content_range = content_range;
        self.accept_ranges = true;
        self
    }
}

/// Weak comparison of an `If-None-Match` list (RFC 9110, section 13.1.2).
pub(crate) fn matches_validator(header: &str, etag: &str) -> bool {
    header.split(',').map(str::trim).any(|candidate| {
        candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate) == etag
    })
}

pub(super) fn system_time_ms() -> Result<u64, ApiResponse> {
    let duration = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| {
            ApiResponse::error(
                500,
                "system_clock_invalid",
                "system clock predates the Unix epoch",
            )
        })?;
    u64::try_from(duration.as_millis()).map_err(|_| {
        ApiResponse::error(
            500,
            "system_clock_invalid",
            "system clock is outside the supported range",
        )
    })
}

pub(crate) fn to_http_response(response: ApiResponse) -> Response<Vec<u8>> {
    let mut result = Response::new(response.body);
    *result.status_mut() =
        StatusCode::from_u16(response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    if result.status() != StatusCode::NOT_MODIFIED {
        result.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static(response.content_type),
        );
        let content_length = HeaderValue::from_str(&result.body().len().to_string())
            .expect("decimal content length is a valid header");
        result.headers_mut().insert(CONTENT_LENGTH, content_length);
    }
    if let Some(etag) = response.etag {
        result.headers_mut().insert(
            ETAG,
            HeaderValue::from_str(&etag).expect("revision validators are valid headers"),
        );
    }
    if let Some(cache_control) = response.cache_control {
        result
            .headers_mut()
            .insert(CACHE_CONTROL, HeaderValue::from_static(cache_control));
    }
    if let Some(allow) = response.allow {
        result
            .headers_mut()
            .insert(ALLOW, HeaderValue::from_static(allow));
    }
    if let Some(challenge) = response.www_authenticate {
        result
            .headers_mut()
            .insert(WWW_AUTHENTICATE, HeaderValue::from_static(challenge));
    }
    if let Some(correlation_id) = response.correlation_id {
        result.headers_mut().insert(
            HeaderName::from_static("x-correlation-id"),
            HeaderValue::from_str(&correlation_id).expect("validated correlation ID is a header"),
        );
    }
    if let Some(content_range) = response.content_range {
        result.headers_mut().insert(
            HeaderName::from_static("content-range"),
            HeaderValue::from_str(&content_range).expect("validated content range is a header"),
        );
    }
    if response.accept_ranges {
        result.headers_mut().insert(
            HeaderName::from_static("accept-ranges"),
            HeaderValue::from_static("bytes"),
        );
    }
    result
}

#[cfg(test)]
mod tests;
