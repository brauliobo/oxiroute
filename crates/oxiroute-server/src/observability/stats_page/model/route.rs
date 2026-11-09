use schemars::JsonSchema;
use serde::Serialize;

use super::{Counter, RouteAction, SelectorKind};

/// A configured HTTP route. The proxy keeps no per-route request counters, so a route carries
/// configuration plus the two counters it does keep (`rescues`, `poolSelections`).
#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RouteStats {
    /// `<service>#<index>`: stable while the configuration keeps the route in place.
    pub(crate) id: String,
    pub(crate) service: String,
    /// Position of the route in its service.
    pub(crate) index: u64,
    /// The listeners that serve the service.
    pub(crate) listeners: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) host: Option<Selector>,
    pub(crate) path: Selector,
    /// Empty means every method.
    pub(crate) methods: Vec<String>,
    pub(crate) action: RouteAction,
    /// The pool a `proxy` route sends to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pool: Option<String>,
    /// The status of a `fixed_response` or `redirect` route.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_retries: Option<u64>,
    /// Requests replayed after the backend answered a rescue status, by that status. Absent until one was
    /// recorded, and absent when several services have a route at this index, because the proxy keys the
    /// counter by index alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rescues: Option<Vec<RescueCount>>,
    /// Requests a header selector sent to a pool; same availability as `rescues`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pool_selections: Option<Vec<PoolSelectionCount>>,
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct Selector {
    pub(crate) kind: SelectorKind,
    pub(crate) value: String,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RescueCount {
    pub(crate) status: u16,
    pub(crate) count: Counter,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PoolSelectionCount {
    pub(crate) pool: String,
    pub(crate) count: Counter,
}
