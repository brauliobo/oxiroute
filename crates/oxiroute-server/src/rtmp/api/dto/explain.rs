use schemars::JsonSchema;
use serde::Serialize;

use crate::{
    ServiceSpec,
    http::action::{HttpActionPlan, HttpRoutePlan, PoolSelectPlan},
    upstream_peer::UpstreamPlan,
};

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RouteExplainResponse {
    host: Option<String>,
    path: String,
    method: String,
    matches: Vec<RouteMatchDto>,
    unmatched: Vec<String>,
}

impl RouteExplainResponse {
    pub(crate) fn new(host: Option<String>, path: String, method: String) -> Self {
        Self {
            host,
            path,
            method,
            matches: Vec::new(),
            unmatched: Vec::new(),
        }
    }

    pub(crate) fn matched(&mut self, service: &ServiceSpec, route: &HttpRoutePlan) {
        self.matches.push(RouteMatchDto::describe(service, route));
    }

    pub(crate) fn unmatched(&mut self, service: &ServiceSpec) {
        self.unmatched.push(service.name.clone());
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct RouteMatchDto {
    listener: String,
    protocol: &'static str,
    bind: String,
    route: String,
    action: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pool: Option<PoolRefDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rescue_pool: Option<PoolRefDto>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    select_pools: Vec<PoolRefDto>,
}

impl RouteMatchDto {
    fn describe(service: &ServiceSpec, route: &HttpRoutePlan) -> Self {
        let mut described = Self {
            listener: service.name.clone(),
            protocol: service.kind.protocol(),
            bind: service.bind.to_string(),
            route: route.route_id.to_string(),
            action: "static",
            status: None,
            pool: None,
            rescue_pool: None,
            select_pools: Vec::new(),
        };
        match &route.action {
            HttpActionPlan::Proxy(proxy) => {
                described.action = "proxy";
                described.pool = Some(PoolRefDto::of(&proxy.pool));
                described.rescue_pool = proxy.rescue_pool.as_deref().map(PoolRefDto::of);
                described.select_pools = proxy
                    .select
                    .iter()
                    .flat_map(PoolSelectPlan::pools)
                    .map(|pool| PoolRefDto::of(pool))
                    .collect();
            }
            HttpActionPlan::Fixed(response) => {
                described.action = "fixed";
                described.status = Some(response.status);
            }
            HttpActionPlan::Redirect(redirect) => {
                described.action = "redirect";
                described.status = Some(redirect.status);
            }
            HttpActionPlan::Static(_) => {}
        }
        described
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct PoolRefDto {
    name: String,
    available_endpoints: usize,
    total_endpoints: usize,
}

impl PoolRefDto {
    fn of(pool: &UpstreamPlan) -> Self {
        let health = pool.selector().health_snapshot();
        Self {
            name: health.name,
            available_endpoints: health.available_endpoints,
            total_endpoints: health.total_endpoints,
        }
    }
}
