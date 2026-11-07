#[allow(clippy::wildcard_imports)]
use super::*;
use crate::inflight::{InflightFilter, InflightView, Phase};

const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 1_000;

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RequestListResponse {
    total: DecimalCounter,
    truncated: bool,
    requests: Vec<InflightView>,
}

struct RequestQuery {
    filter: InflightFilter,
    request_id: Option<String>,
    limit: usize,
}

impl RequestQuery {
    fn parse(query: Option<&str>) -> Result<Self, &'static str> {
        let mut parsed = Self {
            filter: InflightFilter::default(),
            request_id: None,
            limit: DEFAULT_LIMIT,
        };
        for pair in query
            .unwrap_or_default()
            .split('&')
            .filter(|pair| !pair.is_empty())
        {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            match name {
                "pool" => parsed.filter.pool = Some(value.to_owned()),
                "route" => parsed.filter.route = Some(value.to_owned()),
                "requestId" => parsed.request_id = Some(value.to_owned()),
                "phase" => {
                    parsed.filter.phase = Some(Phase::parse(value).ok_or("unknown phase")?);
                }
                "min_age_ms" => {
                    parsed.filter.min_age_ms = value.parse().map_err(|_| "invalid min_age_ms")?;
                }
                "limit" => {
                    parsed.limit = value
                        .parse()
                        .ok()
                        .filter(|limit| (1..=MAX_LIMIT).contains(limit))
                        .ok_or("limit must be between 1 and 1000")?;
                }
                _ => return Err("unknown query parameter"),
            }
        }
        Ok(parsed)
    }
}

impl RequestListResponse {
    /// Live requests, oldest first.
    pub(crate) fn current(query: Option<&str>) -> Result<Self, &'static str> {
        let query = RequestQuery::parse(query)?;
        let mut listing = crate::inflight::registry().list(&query.filter, query.limit);
        if let Some(id) = &query.request_id {
            listing.requests.retain(|view| &view.request_id == id);
            listing.total = listing.requests.len();
        }
        Ok(Self {
            total: (listing.total as u64).into(),
            truncated: listing.total > listing.requests.len(),
            requests: listing.requests,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::inflight::{InflightArrival, registry};

    fn live(id: &str, pool: &str) -> crate::inflight::InflightGuard {
        let guard = registry().register(InflightArrival {
            request_id: id.into(),
            listener: "edge".into(),
            method: "POST".into(),
            host: Some("app.test".into()),
            path: "/v1/render".into(),
            client_ip: None,
            deadline: Duration::from_secs(30),
            access_log: None,
        });
        guard.set_pool(pool);
        guard
    }

    fn render(query: &str) -> serde_json::Value {
        let response = RequestListResponse::current(Some(query)).expect("query");
        serde_json::to_value(response).expect("serializes")
    }

    #[test]
    fn lists_live_requests_with_decimal_counters() {
        let _first = live("dto-a", "dto-pool");
        let _second = live("dto-b", "dto-pool");
        let body = render("pool=dto-pool&limit=1");
        assert_eq!(body["total"], "2");
        assert_eq!(body["truncated"], true);
        assert_eq!(body["requests"].as_array().expect("requests").len(), 1);
        assert_eq!(body["requests"][0]["pool"], "dto-pool");
        assert_eq!(body["requests"][0]["phase"], "routed");
        assert!(body["requests"][0]["ageMs"].is_string());
        assert!(
            body["requests"][0]
                .get("clientIp")
                .is_some_and(|value| value.is_null())
        );
    }

    #[test]
    fn one_request_is_found_by_id_and_unknown_ids_list_nothing() {
        let _guard = live("dto-find-me", "dto-find");
        let body = render("requestId=dto-find-me");
        assert_eq!(body["total"], "1");
        assert_eq!(body["requests"][0]["requestId"], "dto-find-me");
        assert_eq!(render("requestId=missing")["total"], "0");
    }

    #[test]
    fn invalid_queries_are_rejected() {
        for query in [
            "phase=nowhere",
            "limit=0",
            "limit=1001",
            "min_age_ms=x",
            "other=1",
        ] {
            assert!(
                RequestListResponse::current(Some(query)).is_err(),
                "{query}"
            );
        }
    }
}
