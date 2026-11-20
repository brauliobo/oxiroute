#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum EndpointId {
    Status,
    Listeners,
    Pools,
    Servers,
    Generations,
    Requests,
    MediaTicket,
    VodTicket,
    RecordingTicket,
    RtmpSecrets,
    Clients,
    Connections,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AuthPolicy {
    ManagementBearer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ResponseMode {
    Json,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SuccessSchema {
    Status,
    Listeners,
    Pools,
    Servers,
    Generations,
    Requests,
    Ticket,
    RtmpSecrets,
    Clients,
    Connections,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RequestSchema {
    MediaTicket,
    VodTicket,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RequestBody {
    pub(super) schema: RequestSchema,
    pub(super) required: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Failure {
    pub(super) status: &'static str,
    pub(super) description: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct EndpointSpec {
    pub(super) id: EndpointId,
    pub(super) operation_id: &'static str,
    pub(super) path: &'static str,
    pub(super) method: &'static str,
    pub(super) auth: AuthPolicy,
    pub(super) response: ResponseMode,
    pub(super) request: Option<RequestBody>,
    pub(super) success_schema: SuccessSchema,
    pub(super) failures: &'static [Failure],
}

const TICKET_FAILURES: &[Failure] = &[
    Failure {
        status: "400",
        description: "The request body or the requested lifetime is invalid.",
    },
    Failure {
        status: "404",
        description: "The media output or VOD source does not exist.",
    },
];

const RECORDING_TICKET_FAILURES: &[Failure] = &[
    Failure {
        status: "400",
        description: "The request body or the requested lifetime is invalid.",
    },
    Failure {
        status: "404",
        description: "The recording does not exist.",
    },
];

const CLIENT_TRACKING_FAILURES: &[Failure] = &[
    Failure {
        status: "400",
        description: "A query parameter is unknown or invalid.",
    },
    Failure {
        status: "409",
        description: "Client tracking is disabled by the client_tracking configuration.",
    },
];

const ENDPOINTS: [EndpointSpec; 12] = [
    EndpointSpec {
        id: EndpointId::Status,
        operation_id: "getStatus",
        path: "/api/v1/status",
        method: "GET",
        auth: AuthPolicy::ManagementBearer,
        response: ResponseMode::Json,
        request: None,
        failures: &[],
        success_schema: SuccessSchema::Status,
    },
    EndpointSpec {
        id: EndpointId::Listeners,
        operation_id: "getListeners",
        path: "/api/v1/listeners",
        method: "GET",
        auth: AuthPolicy::ManagementBearer,
        response: ResponseMode::Json,
        request: None,
        failures: &[],
        success_schema: SuccessSchema::Listeners,
    },
    EndpointSpec {
        id: EndpointId::Pools,
        operation_id: "getPools",
        path: "/api/v1/pools",
        method: "GET",
        auth: AuthPolicy::ManagementBearer,
        response: ResponseMode::Json,
        request: None,
        failures: &[],
        success_schema: SuccessSchema::Pools,
    },
    EndpointSpec {
        id: EndpointId::Servers,
        operation_id: "getServers",
        path: "/api/v1/servers",
        method: "GET",
        auth: AuthPolicy::ManagementBearer,
        response: ResponseMode::Json,
        request: None,
        failures: &[],
        success_schema: SuccessSchema::Servers,
    },
    EndpointSpec {
        id: EndpointId::Generations,
        operation_id: "getGenerations",
        path: "/api/v1/generations",
        method: "GET",
        auth: AuthPolicy::ManagementBearer,
        response: ResponseMode::Json,
        request: None,
        failures: &[],
        success_schema: SuccessSchema::Generations,
    },
    EndpointSpec {
        id: EndpointId::Requests,
        operation_id: "getRequests",
        path: "/api/v1/requests",
        method: "GET",
        auth: AuthPolicy::ManagementBearer,
        response: ResponseMode::Json,
        request: None,
        failures: &[],
        success_schema: SuccessSchema::Requests,
    },
    EndpointSpec {
        id: EndpointId::MediaTicket,
        operation_id: "createMediaTicket",
        path: "/api/v1/rtmp/media/{service}/{application}/{stream}/ticket",
        method: "POST",
        auth: AuthPolicy::ManagementBearer,
        response: ResponseMode::Json,
        request: Some(RequestBody {
            schema: RequestSchema::MediaTicket,
            required: false,
        }),
        failures: TICKET_FAILURES,
        success_schema: SuccessSchema::Ticket,
    },
    EndpointSpec {
        id: EndpointId::VodTicket,
        operation_id: "createVodTicket",
        path: "/api/v1/rtmp/vod/{service}/{application}/{source}/ticket",
        method: "POST",
        auth: AuthPolicy::ManagementBearer,
        response: ResponseMode::Json,
        request: Some(RequestBody {
            schema: RequestSchema::VodTicket,
            required: true,
        }),
        failures: TICKET_FAILURES,
        success_schema: SuccessSchema::Ticket,
    },
    EndpointSpec {
        id: EndpointId::RecordingTicket,
        operation_id: "createRecordingTicket",
        path: "/api/v1/rtmp/recordings/{recordingId}/ticket",
        method: "POST",
        auth: AuthPolicy::ManagementBearer,
        response: ResponseMode::Json,
        request: Some(RequestBody {
            schema: RequestSchema::MediaTicket,
            required: false,
        }),
        failures: RECORDING_TICKET_FAILURES,
        success_schema: SuccessSchema::Ticket,
    },
    EndpointSpec {
        id: EndpointId::RtmpSecrets,
        operation_id: "listRtmpSecretFingerprints",
        path: "/api/v1/rtmp/secrets",
        method: "GET",
        auth: AuthPolicy::ManagementBearer,
        response: ResponseMode::Json,
        request: None,
        failures: &[],
        success_schema: SuccessSchema::RtmpSecrets,
    },
    EndpointSpec {
        id: EndpointId::Clients,
        operation_id: "getClients",
        path: "/api/v1/clients",
        method: "GET",
        auth: AuthPolicy::ManagementBearer,
        response: ResponseMode::Json,
        request: None,
        failures: CLIENT_TRACKING_FAILURES,
        success_schema: SuccessSchema::Clients,
    },
    EndpointSpec {
        id: EndpointId::Connections,
        operation_id: "getConnections",
        path: "/api/v1/connections",
        method: "GET",
        auth: AuthPolicy::ManagementBearer,
        response: ResponseMode::Json,
        request: None,
        failures: CLIENT_TRACKING_FAILURES,
        success_schema: SuccessSchema::Connections,
    },
];

pub(super) fn all() -> &'static [EndpointSpec] {
    &ENDPOINTS
}

impl EndpointSpec {
    fn matches(&self, path: &str) -> bool {
        let mut template = self.path.split('/');
        let mut actual = path.split('/');
        loop {
            match (template.next(), actual.next()) {
                (None, None) => return true,
                (Some(expected), Some(segment)) if expected.starts_with('{') => {
                    if segment.is_empty() {
                        return false;
                    }
                }
                (Some(expected), Some(segment)) if expected == segment => {}
                _ => return false,
            }
        }
    }
}

pub(super) fn match_path(path: &str) -> Option<EndpointId> {
    all()
        .iter()
        .find(|endpoint| endpoint.matches(path))
        .map(|endpoint| endpoint.id)
}

pub(super) fn match_path_and_query(path_and_query: &str) -> Option<EndpointId> {
    let path = path_and_query
        .split_once('?')
        .map_or(path_and_query, |(path, _)| path);
    match_path(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_owns_the_protected_read_only_management_endpoints() {
        assert_eq!(all().len(), 12);
        for (endpoint, expected) in all().iter().take(6).zip([
            (
                EndpointId::Status,
                "getStatus",
                "/api/v1/status",
                SuccessSchema::Status,
            ),
            (
                EndpointId::Listeners,
                "getListeners",
                "/api/v1/listeners",
                SuccessSchema::Listeners,
            ),
            (
                EndpointId::Pools,
                "getPools",
                "/api/v1/pools",
                SuccessSchema::Pools,
            ),
            (
                EndpointId::Servers,
                "getServers",
                "/api/v1/servers",
                SuccessSchema::Servers,
            ),
            (
                EndpointId::Generations,
                "getGenerations",
                "/api/v1/generations",
                SuccessSchema::Generations,
            ),
            (
                EndpointId::Requests,
                "getRequests",
                "/api/v1/requests",
                SuccessSchema::Requests,
            ),
        ]) {
            assert_eq!(
                (endpoint.id, endpoint.operation_id, endpoint.path),
                (expected.0, expected.1, expected.2)
            );
            assert_eq!(endpoint.method, "GET");
            assert_eq!(endpoint.auth, AuthPolicy::ManagementBearer);
            assert_eq!(endpoint.response, ResponseMode::Json);
            assert_eq!(endpoint.success_schema, expected.3);
            assert_eq!(match_path(endpoint.path), Some(endpoint.id));
        }
    }

    #[test]
    fn registry_owns_the_ticket_minting_endpoints() {
        assert_eq!(
            match_path("/api/v1/rtmp/media/live/cam/main/ticket"),
            Some(EndpointId::MediaTicket)
        );
        assert_eq!(
            match_path("/api/v1/rtmp/vod/live/cam/archive/ticket"),
            Some(EndpointId::VodTicket)
        );
        assert_eq!(
            match_path("/api/v1/rtmp/recordings/0123abcd/ticket"),
            Some(EndpointId::RecordingTicket)
        );
        for (endpoint, placeholders) in all()[6..9].iter().zip([3, 3, 1]) {
            assert_eq!(endpoint.method, "POST");
            assert_eq!(endpoint.path.matches('{').count(), placeholders);
        }
        for path in [
            "/api/v1/rtmp/media/live/cam/ticket",
            "/api/v1/rtmp/media/live/cam/main/index.m3u8",
            "/api/v1/rtmp/media/live//main/ticket",
            "/api/v1/rtmp/media/live/cam/main/ticket/extra",
            "/api/v1/rtmp/vod/live/cam/archive/a/b/ticket",
            "/api/v1/rtmp/recordings/ticket",
            "/api/v1/rtmp/recordings//ticket",
            "/api/v1/rtmp/recordings/0123abcd/content",
            "/api/v1/rtmp/recordings/0123abcd/ticket/extra",
            "/api/v1/rtmp/secrets/live",
        ] {
            assert_eq!(match_path(path), None, "unexpected endpoint match: {path}");
        }
    }

    #[test]
    fn registry_owns_the_secret_fingerprint_listing() {
        let endpoint = &all()[9];
        assert_eq!(endpoint.id, EndpointId::RtmpSecrets);
        assert_eq!(endpoint.operation_id, "listRtmpSecretFingerprints");
        assert_eq!(endpoint.method, "GET");
        assert_eq!(endpoint.auth, AuthPolicy::ManagementBearer);
        assert_eq!(match_path(endpoint.path), Some(EndpointId::RtmpSecrets));
    }

    #[test]
    fn registry_accepts_queries_but_rejects_nearby_paths() {
        assert_eq!(
            match_path_and_query("/api/v1/status?verbose=true"),
            Some(EndpointId::Status)
        );
        assert_eq!(
            match_path_and_query("/api/v1/generations?limit=5"),
            Some(EndpointId::Generations)
        );
        assert_eq!(
            match_path_and_query("/api/v1/requests?pool=farm&limit=5"),
            Some(EndpointId::Requests)
        );
        for path in [
            "/api/v1/status/",
            "/api/v1/requests/abc",
            "/api/v1/listeners/administrative-state",
            "/api/v1/topology",
            "/api/v1/events",
            "/ready",
            "/metrics",
            "/api/v1/rtmp/streams",
        ] {
            assert_eq!(match_path(path), None, "unexpected endpoint match: {path}");
        }
    }
}
