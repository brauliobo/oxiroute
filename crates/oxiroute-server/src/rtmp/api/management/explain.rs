use http::Method;
use oxiroute_config::is_unambiguous_http_path;

use super::{ApiResponse, ManagementState, dto_response};
use crate::{
    ServiceKind, rtmp::api::dto::RouteExplainResponse, rtmp::api::recordings::percent_decode,
};

struct Question {
    listener: Option<String>,
    host: Option<String>,
    path: String,
    method: Method,
}

impl Question {
    fn parse(query: Option<&str>) -> Result<Self, &'static str> {
        let mut listener = None;
        let mut host = None;
        let mut path = None;
        let mut method = Method::GET;
        for pair in query
            .unwrap_or_default()
            .split('&')
            .filter(|pair| !pair.is_empty())
        {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let value = percent_decode(value).ok_or("query value is not valid percent-encoding")?;
            match key {
                "listener" => listener = Some(value),
                "host" => host = Some(value),
                "path" => path = Some(value),
                "method" => {
                    method =
                        Method::from_bytes(value.as_bytes()).map_err(|_| "method is invalid")?;
                }
                _ => return Err("unknown query parameter"),
            }
        }
        let path = path.ok_or("path is required")?;
        if !path.starts_with('/') || !is_unambiguous_http_path(&path) {
            return Err("path must be an absolute, unambiguous HTTP path");
        }
        Ok(Self {
            listener,
            host,
            path,
            method,
        })
    }
}

impl ManagementState {
    pub(super) fn route_explain(&self, query: Option<&str>) -> ApiResponse {
        let question = match Question::parse(query) {
            Ok(question) => question,
            Err(message) => return ApiResponse::error(400, "invalid_route_query", message),
        };
        let active = match self.active() {
            Ok(active) => active,
            Err(response) => return response,
        };
        let services = active.services().iter().filter(|service| {
            question
                .listener
                .as_ref()
                .is_none_or(|name| name == &service.name)
        });
        let mut response = RouteExplainResponse::new(
            question.host.clone(),
            question.path.clone(),
            question.method.to_string(),
        );
        let mut http_services = 0;
        for service in services {
            let (ServiceKind::Http(plan) | ServiceKind::Http3(plan)) = &service.kind else {
                continue;
            };
            http_services += 1;
            match plan.select_route_path(question.host.as_deref(), &question.path, &question.method)
            {
                Some(route) => response.matched(service, route),
                None => response.unmatched(service),
            }
        }
        if http_services == 0 {
            return ApiResponse::error(
                404,
                "listener_not_found",
                "no HTTP listener of the active generation matches",
            );
        }
        dto_response(200, &response)
    }
}

#[cfg(test)]
mod tests;
