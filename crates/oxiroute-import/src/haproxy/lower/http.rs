use std::collections::{HashMap, HashSet};

use oxiroute_config::{
    HttpHostSelector, HttpLiteralHeader, HttpPathSelector, HttpProxyPolicy, HttpRedirectLocation,
    HttpRequestHeaderValue, HttpRoute, HttpRouteAction, HttpRoutePolicy, HttpService,
};

use crate::ProvenanceSpan;

use super::{Lowerer, Representability};
use crate::haproxy::{
    AclCriterion, AclDefinition, BackendReference, ConditionPolarity, EffectiveFrontend,
    EffectiveListen, EffectiveSection, EffectiveValue, HttpRequestRule, SectionId, UseBackend,
};

use super::provenance::{
    CanonicalPath, deduplicate_sources, extend_sources, provenance_sources, section_sources,
};

pub(super) struct LoweredHttpService {
    service: HttpService,
    sources: Vec<ProvenanceSpan>,
    routes: Vec<Vec<ProvenanceSpan>>,
}

mod frontend;
mod matcher;
mod routes;
mod terminal;

use matcher::{
    LoweredRoute, RouteMatcher, deduplicate_routes, has_conditional_terminal_rule,
    is_terminal_rule, matchers_overlap, merge_route_matchers, proxy_action, route_pool,
};
