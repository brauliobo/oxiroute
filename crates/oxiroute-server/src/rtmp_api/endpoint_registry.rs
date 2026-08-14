#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum EndpointId {
    Status,
    Listeners,
    Pools,
    Servers,
    Generations,
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct EndpointSpec {
    pub(super) id: EndpointId,
    pub(super) operation_id: &'static str,
    pub(super) path: &'static str,
    pub(super) method: &'static str,
    pub(super) auth: AuthPolicy,
    pub(super) response: ResponseMode,
    pub(super) success_schema: SuccessSchema,
}

const ENDPOINTS: [EndpointSpec; 5] = [
    EndpointSpec {
        id: EndpointId::Status,
        operation_id: "getStatus",
        path: "/api/v1/status",
        method: "GET",
        auth: AuthPolicy::ManagementBearer,
        response: ResponseMode::Json,
        success_schema: SuccessSchema::Status,
    },
    EndpointSpec {
        id: EndpointId::Listeners,
        operation_id: "getListeners",
        path: "/api/v1/listeners",
        method: "GET",
        auth: AuthPolicy::ManagementBearer,
        response: ResponseMode::Json,
        success_schema: SuccessSchema::Listeners,
    },
    EndpointSpec {
        id: EndpointId::Pools,
        operation_id: "getPools",
        path: "/api/v1/pools",
        method: "GET",
        auth: AuthPolicy::ManagementBearer,
        response: ResponseMode::Json,
        success_schema: SuccessSchema::Pools,
    },
    EndpointSpec {
        id: EndpointId::Servers,
        operation_id: "getServers",
        path: "/api/v1/servers",
        method: "GET",
        auth: AuthPolicy::ManagementBearer,
        response: ResponseMode::Json,
        success_schema: SuccessSchema::Servers,
    },
    EndpointSpec {
        id: EndpointId::Generations,
        operation_id: "getGenerations",
        path: "/api/v1/generations",
        method: "GET",
        auth: AuthPolicy::ManagementBearer,
        response: ResponseMode::Json,
        success_schema: SuccessSchema::Generations,
    },
];

pub(super) fn all() -> &'static [EndpointSpec; 5] {
    &ENDPOINTS
}

pub(super) fn match_path(path: &str) -> Option<EndpointId> {
    all()
        .iter()
        .find(|endpoint| endpoint.path == path)
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
        assert_eq!(all().len(), 5);
        for (endpoint, expected) in all().iter().zip([
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
    fn registry_accepts_queries_but_rejects_nearby_paths() {
        assert_eq!(
            match_path_and_query("/api/v1/status?verbose=true"),
            Some(EndpointId::Status)
        );
        assert_eq!(
            match_path_and_query("/api/v1/generations?limit=5"),
            Some(EndpointId::Generations)
        );
        for path in [
            "/api/v1/status/",
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
