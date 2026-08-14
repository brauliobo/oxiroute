use std::time::SystemTime;

use http::{
    HeaderValue, Response, StatusCode,
    header::{ALLOW, CONTENT_LENGTH, CONTENT_TYPE, HeaderName, WWW_AUTHENTICATE},
};
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
        }
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
    result.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static(response.content_type),
    );
    let content_length = HeaderValue::from_str(&result.body().len().to_string())
        .expect("decimal content length is a valid header");
    result.headers_mut().insert(CONTENT_LENGTH, content_length);
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
mod tests {
    use super::{ApiResponse, to_http_response};

    #[test]
    fn error_responses_preserve_status_body_and_headers() {
        let cases = [
            (
                ApiResponse::error(
                    503,
                    "status_unavailable",
                    "runtime status could not be sampled",
                )
                .with_correlation("request-42".into()),
                503,
                r#"{"error":{"code":"status_unavailable","message":"runtime status could not be sampled"}}"#,
                None,
                None,
                Some("request-42"),
            ),
            (
                ApiResponse::route_not_found(),
                404,
                r#"{"error":{"code":"route_not_found","message":"route does not exist"}}"#,
                None,
                None,
                None,
            ),
            (
                ApiResponse::method_not_allowed("GET"),
                405,
                r#"{"error":{"code":"method_not_allowed","message":"method is not allowed"}}"#,
                Some("GET"),
                None,
                None,
            ),
            (
                ApiResponse::unauthorized(),
                401,
                r#"{"error":{"code":"unauthorized","message":"a valid Bearer management token is required"}}"#,
                None,
                Some("Bearer"),
                None,
            ),
            (
                ApiResponse::error(
                    422,
                    "code\"\\\n\0雪",
                    "message \"quoted\" \\ path\n\t\u{0008}\u{000c}\r\0雪",
                ),
                422,
                r#"{"error":{"code":"code\"\\\n\u0000雪","message":"message \"quoted\" \\ path\n\t\b\f\r\u0000雪"}}"#,
                None,
                None,
                None,
            ),
        ];

        for (response, status, body, allow, challenge, correlation) in cases {
            assert_eq!(response.status, status);
            assert_eq!(response.body, body.as_bytes());
            assert_eq!(response.content_type, "application/json");

            let response = to_http_response(response);
            assert_eq!(response.status().as_u16(), status);
            assert_eq!(response.body(), body.as_bytes());
            assert_eq!(response.headers()["content-type"], "application/json");
            assert_eq!(response.headers()["content-length"], body.len().to_string());
            assert_eq!(
                response
                    .headers()
                    .get("allow")
                    .map(|value| value.to_str().unwrap()),
                allow
            );
            assert_eq!(
                response
                    .headers()
                    .get("www-authenticate")
                    .map(|value| value.to_str().unwrap()),
                challenge
            );
            assert_eq!(
                response
                    .headers()
                    .get("x-correlation-id")
                    .map(|value| value.to_str().unwrap()),
                correlation
            );
        }
    }

    #[test]
    fn correlation_id_is_written_as_a_response_header() {
        let response = to_http_response(
            ApiResponse::json(200, &serde_json::json!({ "ok": true }))
                .with_correlation("request-42".into()),
        );

        assert_eq!(response.headers()["x-correlation-id"], "request-42");
    }
}
