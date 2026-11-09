use std::collections::HashMap;

use oxiroute_config::{
    ConfigDraft, HttpHostSelector, HttpPathSelector, HttpRoute, HttpRouteAction, HttpService,
    Protocol,
};

use super::super::model::{
    PoolSelectionCount, RescueCount, RouteAction, RouteStats, Selector, SelectorKind,
};
use crate::RuntimeSnapshot;

pub(super) fn routes(runtime: &RuntimeSnapshot, config: &ConfigDraft) -> Vec<RouteStats> {
    let mut services_per_index = HashMap::<usize, usize>::new();
    for service in &config.http_services {
        for index in 0..service.routes.len() {
            *services_per_index.entry(index).or_default() += 1;
        }
    }
    config
        .http_services
        .iter()
        .flat_map(|service| {
            let listeners = http_listeners(config, service);
            let services_per_index = &services_per_index;
            service
                .routes
                .iter()
                .enumerate()
                .map(move |(index, route)| {
                    let mut stats = route_stats(service, index, route, listeners.clone());
                    if services_per_index[&index] == 1 {
                        attach_counters(&mut stats, runtime, index);
                    }
                    stats
                })
        })
        .collect()
}

fn http_listeners(config: &ConfigDraft, service: &HttpService) -> Vec<String> {
    config
        .listeners
        .iter()
        .filter(|listener| {
            matches!(listener.protocol, Protocol::Http | Protocol::Http3)
                && listener.service.as_deref() == Some(service.name.as_str())
        })
        .map(|listener| listener.name.clone())
        .collect()
}

fn route_stats(
    service: &HttpService,
    index: usize,
    route: &HttpRoute,
    listeners: Vec<String>,
) -> RouteStats {
    let (action, pool, status, max_retries) = match &route.action {
        HttpRouteAction::Proxy {
            upstream_pool,
            policy,
        } => (
            RouteAction::Proxy,
            Some(upstream_pool.clone()),
            None,
            Some(u64::from(policy.retry.max_retries)),
        ),
        HttpRouteAction::FixedResponse { status, .. } => {
            (RouteAction::FixedResponse, None, Some(*status), None)
        }
        HttpRouteAction::Redirect { status, .. } => {
            (RouteAction::Redirect, None, Some(*status), None)
        }
        HttpRouteAction::StaticFiles { .. } => (RouteAction::StaticFiles, None, None, None),
    };
    RouteStats {
        id: format!("{}#{index}", service.name),
        service: service.name.clone(),
        index: index as u64,
        listeners,
        host: route.host.as_ref().map(host_selector),
        path: path_selector(&route.path),
        methods: route.methods.clone(),
        action,
        pool,
        status,
        max_retries,
        rescues: None,
        pool_selections: None,
    }
}

fn attach_counters(stats: &mut RouteStats, runtime: &RuntimeSnapshot, index: usize) {
    let key = index.to_string();
    let rescues = runtime
        .process
        .rescues
        .iter()
        .filter(|rescue| rescue.route == key)
        .map(|rescue| RescueCount {
            status: rescue.status,
            count: rescue.count.into(),
        })
        .collect::<Vec<_>>();
    let selections = runtime
        .process
        .pool_selections
        .iter()
        .filter(|selection| selection.route == key)
        .map(|selection| PoolSelectionCount {
            pool: selection.pool.clone(),
            count: selection.count.into(),
        })
        .collect::<Vec<_>>();
    stats.rescues = (!rescues.is_empty()).then_some(rescues);
    stats.pool_selections = (!selections.is_empty()).then_some(selections);
}

fn host_selector(selector: &HttpHostSelector) -> Selector {
    let (kind, value) = match selector {
        HttpHostSelector::NormalizedHost { value } => (SelectorKind::NormalizedHost, value),
        HttpHostSelector::ExactAuthority { value } => (SelectorKind::ExactAuthority, value),
        HttpHostSelector::AsciiCaseInsensitiveExactAuthority { value } => {
            (SelectorKind::AsciiCaseInsensitiveExactAuthority, value)
        }
        HttpHostSelector::NginxLeadingWildcard { value } => {
            (SelectorKind::NginxLeadingWildcard, value)
        }
        HttpHostSelector::NginxLeadingDot { value } => (SelectorKind::NginxLeadingDot, value),
    };
    Selector {
        kind,
        value: value.clone(),
    }
}

fn path_selector(selector: &HttpPathSelector) -> Selector {
    let (kind, value) = match selector {
        HttpPathSelector::SegmentPrefix { value } => (SelectorKind::SegmentPrefix, value),
        HttpPathSelector::RawPrefix { value } => (SelectorKind::RawPrefix, value),
        HttpPathSelector::Exact { value } => (SelectorKind::Exact, value),
        HttpPathSelector::AsciiCaseInsensitiveExact { value } => {
            (SelectorKind::AsciiCaseInsensitiveExact, value)
        }
    };
    Selector {
        kind,
        value: value.clone(),
    }
}
