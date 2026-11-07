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
