#[allow(clippy::wildcard_imports)]
use super::*;

impl ManagementState {
    pub(super) fn listeners(&self) -> ApiResponse {
        match self.metrics.snapshot() {
            Ok(snapshot) => ApiResponse::json(
                200,
                &serde_json::to_value(ListenerInventoryResponse::from(snapshot))
                    .expect("listener inventory DTO serializes"),
            ),
            Err(_) => ApiResponse::error(
                503,
                "listeners_unavailable",
                "listener state is unavailable",
            ),
        }
    }

    pub(super) fn pools(&self) -> ApiResponse {
        let active = match self.active() {
            Ok(active) => active,
            Err(response) => return response,
        };
        let pools = active
            .pools()
            .iter()
            .map(|pool| pool.health_snapshot())
            .collect::<Vec<_>>();
        ApiResponse::json(
            200,
            &serde_json::to_value(PoolInventoryResponse::from(pools))
                .expect("pool inventory DTO serializes"),
        )
    }

    pub(super) fn servers(&self) -> ApiResponse {
        let active = match self.active() {
            Ok(active) => active,
            Err(response) => return response,
        };
        let pools = active
            .pools()
            .iter()
            .map(|pool| pool.health_snapshot())
            .collect::<Vec<_>>();
        ApiResponse::json(
            200,
            &serde_json::to_value(ServerInventoryResponse::from(pools))
                .expect("server inventory DTO serializes"),
        )
    }

    pub(super) fn audit_status(&self) -> ApiResponse {
        dto_response(200, &AuditStatusResponse::from(self.audit.status()))
    }

    pub(super) fn audit(&self, query: Option<&str>) -> ApiResponse {
        let mut after = 0_u64;
        let mut limit = 100_usize;
        let mut category = None;
        let mut result = None;
        if let Some(query) = query {
            for pair in query.split('&') {
                let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
                match name {
                    "after" => match value.parse() {
                        Ok(value) => after = value,
                        Err(_) => {
                            return ApiResponse::error(
                                400,
                                "invalid_cursor",
                                "audit cursor is invalid",
                            );
                        }
                    },
                    "limit" => match value.parse::<usize>() {
                        Ok(value) if (1..=MAX_EVENT_LIMIT).contains(&value) => limit = value,
                        _ => {
                            return ApiResponse::error(
                                400,
                                "invalid_limit",
                                "audit limit must be between 1 and 1000",
                            );
                        }
                    },
                    "category" => match AuditCategory::parse(value) {
                        Some(value) => category = Some(value),
                        None => {
                            return ApiResponse::error(
                                400,
                                "invalid_category",
                                "audit category is invalid",
                            );
                        }
                    },
                    "result" => match AuditResult::parse(value) {
                        Some(value) => result = Some(value),
                        None => {
                            return ApiResponse::error(
                                400,
                                "invalid_result",
                                "audit result is invalid",
                            );
                        }
                    },
                    _ => {
                        return ApiResponse::error(
                            400,
                            "invalid_query",
                            "audit query parameter is invalid",
                        );
                    }
                }
            }
        }
        let page = self.audit.page(after, limit, category, result);
        dto_response(200, &AuditPageResponse::from(page))
    }

    pub(super) fn events(version: EventApiVersion, query: Option<&str>) -> ApiResponse {
        let mut after = 0_u64;
        let mut limit = 100_usize;
        if let Some(query) = query {
            for pair in query.split('&') {
                let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
                match name {
                    "after" => match value.parse() {
                        Ok(value) => after = value,
                        Err(_) => {
                            return ApiResponse::error(
                                400,
                                "invalid_cursor",
                                "event cursor is invalid",
                            );
                        }
                    },
                    "limit" => match value.parse::<usize>() {
                        Ok(value) if (1..=MAX_EVENT_LIMIT).contains(&value) => limit = value,
                        _ => {
                            return ApiResponse::error(
                                400,
                                "invalid_limit",
                                "event limit must be between 1 and 1000",
                            );
                        }
                    },
                    _ => {
                        return ApiResponse::error(
                            400,
                            "invalid_query",
                            "event query parameter is invalid",
                        );
                    }
                }
            }
        }
        let page = crate::observability::operational_event::page(after, limit);
        match version {
            EventApiVersion::V1 => dto_response(200, &EventPageV1Response::from(page)),
            EventApiVersion::V2 => dto_response(200, &EventPageV2Response::from(page)),
        }
    }
}
