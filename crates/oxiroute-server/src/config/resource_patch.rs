use oxiroute_config::{ConfigDraft, HttpRoute, UpstreamServer};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Declarative top-level resource collections in [`ConfigDraft`].
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ResourceCollection {
    Listeners,
    UpstreamPools,
    HttpServices,
    Certificates,
    TlsProfiles,
    CacheStores,
    ForwardProxyServices,
    RtmpServices,
    L4Services,
}

impl ResourceCollection {
    pub(crate) const fn json_key(self) -> &'static str {
        match self {
            Self::Listeners => "listeners",
            Self::UpstreamPools => "upstream_pools",
            Self::HttpServices => "http_services",
            Self::Certificates => "certificates",
            Self::TlsProfiles => "tls_profiles",
            Self::CacheStores => "cache_stores",
            Self::ForwardProxyServices => "forward_proxy_services",
            Self::RtmpServices => "rtmp_services",
            Self::L4Services => "l4_services",
        }
    }

    pub(crate) fn from_cli_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "listener" | "listeners" => Some(Self::Listeners),
            "pool" | "pools" | "upstream" | "upstream_pool" | "upstream_pools" => {
                Some(Self::UpstreamPools)
            }
            "http" | "http_service" | "http_services" | "service" | "services" => {
                Some(Self::HttpServices)
            }
            "cert" | "certs" | "certificate" | "certificates" => Some(Self::Certificates),
            "tls" | "tls_profile" | "tls_profiles" => Some(Self::TlsProfiles),
            "cache" | "cache_store" | "cache_stores" => Some(Self::CacheStores),
            "forward" | "forward_proxy" | "forward_proxy_service" | "forward_proxy_services" => {
                Some(Self::ForwardProxyServices)
            }
            "rtmp" | "rtmp_service" | "rtmp_services" => Some(Self::RtmpServices),
            "l4" | "l4_service" | "l4_services" | "tcp" | "udp" => Some(Self::L4Services),
            _ => None,
        }
    }
}

/// A declarative configuration mutation applied to a [`ConfigDraft`] before validation and
/// format-preserving persistence.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ResourcePatch {
    /// Sets one or more root-level scalar or block fields on `ConfigDraft`.
    SetGlobal { fields: Map<String, Value> },
    /// Inserts (or replaces when `replace = true`) one named item in a top-level collection.
    Upsert {
        collection: ResourceCollection,
        item: Value,
        #[serde(default)]
        replace: bool,
    },
    /// Removes one named item from a top-level collection.
    Remove {
        collection: ResourceCollection,
        name: String,
    },
    /// Inserts or replaces a static server inside an existing `upstream_pools` entry.
    UpsertPoolServer {
        pool: String,
        server: Value,
        #[serde(default)]
        replace: bool,
    },
    /// Removes a static server by name from an existing `upstream_pools` entry.
    RemovePoolServer { pool: String, name: String },
    /// Adds (or replaces at `index`) an HTTP route on an existing `http_services` entry.
    UpsertHttpRoute {
        service: String,
        route: Value,
        #[serde(default)]
        index: Option<usize>,
    },
    /// Removes an HTTP route by zero-based index from an existing `http_services` entry.
    RemoveHttpRoute { service: String, index: usize },
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ResourcePatchError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Invalid(String),
}

const ALLOWED_GLOBAL_KEYS: &[&str] = &[
    "max_connections",
    "listener_workers",
    "http_fast_lane",
    "http_data_plane",
    "connection_scheduling",
    "connection_age_ms",
    "worker_scaling",
    "min_active_workers",
    "target_busy_percent",
    "scale_down_delay_ms",
    "tls_handshake_threads",
    "http3_congestion_control",
    "http3_segmentation_offload",
    "retirement",
    "management",
    "log_filter",
    "cpu_level",
    "audit",
    "config_history",
    "client_tracking",
    "registry",
    "stats",
];

/// Applies one declarative [`ResourcePatch`] to a [`ConfigDraft`], returning the updated draft.
pub(crate) fn apply_patch(
    draft: &ConfigDraft,
    patch: ResourcePatch,
) -> Result<ConfigDraft, ResourcePatchError> {
    match patch {
        ResourcePatch::SetGlobal { fields } => apply_set_global(draft, fields),
        ResourcePatch::Upsert {
            collection,
            item,
            replace,
        } => apply_upsert(draft, collection, item, replace),
        ResourcePatch::Remove { collection, name } => apply_remove(draft, collection, &name),
        ResourcePatch::UpsertPoolServer {
            pool,
            server,
            replace,
        } => apply_upsert_pool_server(draft, &pool, server, replace),
        ResourcePatch::RemovePoolServer { pool, name } => {
            apply_remove_pool_server(draft, &pool, &name)
        }
        ResourcePatch::UpsertHttpRoute {
            service,
            route,
            index,
        } => apply_upsert_http_route(draft, &service, route, index),
        ResourcePatch::RemoveHttpRoute { service, index } => {
            apply_remove_http_route(draft, &service, index)
        }
    }
}

/// Lists all named items in a top-level collection as JSON values.
pub(crate) fn list_collection(
    draft: &ConfigDraft,
    collection: ResourceCollection,
) -> Result<Vec<Value>, ResourcePatchError> {
    let value = serde_json::to_value(draft)
        .map_err(|err| ResourcePatchError::Invalid(format!("cannot serialize draft: {err}")))?;
    let items = value
        .get(collection.json_key())
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(items)
}

/// Finds a single named item in a top-level collection.
pub(crate) fn get_collection_item(
    draft: &ConfigDraft,
    collection: ResourceCollection,
    name: &str,
) -> Result<Value, ResourcePatchError> {
    list_collection(draft, collection)?
        .into_iter()
        .find(|item| item.get("name").and_then(Value::as_str) == Some(name))
        .ok_or_else(|| {
            ResourcePatchError::NotFound(format!(
                "{} `{name}` was not found",
                collection.json_key()
            ))
        })
}

fn apply_set_global(
    draft: &ConfigDraft,
    fields: Map<String, Value>,
) -> Result<ConfigDraft, ResourcePatchError> {
    if fields.is_empty() {
        return Err(ResourcePatchError::Invalid(
            "set_global requires at least one field".to_owned(),
        ));
    }
    let mut root = draft_to_object(draft)?;
    for (key, value) in fields {
        if !ALLOWED_GLOBAL_KEYS.contains(&key.as_str()) {
            return Err(ResourcePatchError::Invalid(format!(
                "unsupported global setting `{key}`"
            )));
        }
        if value.is_null() && key != "max_connections" {
            root.remove(&key);
        } else {
            root.insert(key, value);
        }
    }
    object_to_draft(root)
}

fn apply_upsert(
    draft: &ConfigDraft,
    collection: ResourceCollection,
    item: Value,
    replace: bool,
) -> Result<ConfigDraft, ResourcePatchError> {
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            ResourcePatchError::Invalid(format!(
                "{} item must be an object with a non-empty `name` field",
                collection.json_key()
            ))
        })?
        .to_owned();

    let mut root = draft_to_object(draft)?;
    let list = root
        .entry(collection.json_key().to_owned())
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| {
            ResourcePatchError::Invalid(format!("{} is not an array", collection.json_key()))
        })?;

    if let Some(pos) = list
        .iter()
        .position(|existing| existing.get("name").and_then(Value::as_str) == Some(name.as_str()))
    {
        if !replace {
            return Err(ResourcePatchError::Conflict(format!(
                "{} `{name}` already exists; pass replace=true to overwrite",
                collection.json_key()
            )));
        }
        list[pos] = item;
    } else {
        list.push(item);
    }

    object_to_draft(root)
}

fn apply_remove(
    draft: &ConfigDraft,
    collection: ResourceCollection,
    name: &str,
) -> Result<ConfigDraft, ResourcePatchError> {
    let mut root = draft_to_object(draft)?;
    let list = root
        .entry(collection.json_key().to_owned())
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| {
            ResourcePatchError::Invalid(format!("{} is not an array", collection.json_key()))
        })?;

    let before = list.len();
    list.retain(|existing| existing.get("name").and_then(Value::as_str) != Some(name));
    if list.len() == before {
        return Err(ResourcePatchError::NotFound(format!(
            "{} `{name}` was not found",
            collection.json_key()
        )));
    }
    object_to_draft(root)
}

fn apply_upsert_pool_server(
    draft: &ConfigDraft,
    pool_name: &str,
    server_value: Value,
    replace: bool,
) -> Result<ConfigDraft, ResourcePatchError> {
    let server: UpstreamServer = serde_json::from_value(server_value)
        .map_err(|err| ResourcePatchError::Invalid(format!("invalid upstream server: {err}")))?;
    let mut updated = draft.clone();
    let pool = updated
        .upstream_pools
        .iter_mut()
        .find(|pool| pool.name == pool_name)
        .ok_or_else(|| {
            ResourcePatchError::NotFound(format!("upstream pool `{pool_name}` was not found"))
        })?;
    if let Some(pos) = pool.servers.iter().position(|s| s.name == server.name) {
        if !replace {
            return Err(ResourcePatchError::Conflict(format!(
                "server `{}` already exists in pool `{pool_name}`",
                server.name
            )));
        }
        pool.servers[pos] = server;
    } else {
        pool.servers.push(server);
        if let oxiroute_config::UpstreamAlgorithm::WeightedRoundRobin { weights } =
            &mut pool.algorithm
        {
            weights.push(1);
        }
    }
    Ok(updated)
}

fn apply_remove_pool_server(
    draft: &ConfigDraft,
    pool_name: &str,
    server_name: &str,
) -> Result<ConfigDraft, ResourcePatchError> {
    let mut updated = draft.clone();
    let pool = updated
        .upstream_pools
        .iter_mut()
        .find(|pool| pool.name == pool_name)
        .ok_or_else(|| {
            ResourcePatchError::NotFound(format!("upstream pool `{pool_name}` was not found"))
        })?;
    let Some(pos) = pool.servers.iter().position(|s| s.name == server_name) else {
        return Err(ResourcePatchError::NotFound(format!(
            "server `{server_name}` was not found in pool `{pool_name}`"
        )));
    };
    pool.servers.remove(pos);
    if let oxiroute_config::UpstreamAlgorithm::WeightedRoundRobin { weights } = &mut pool.algorithm
        && pos < weights.len()
    {
        weights.remove(pos);
    }
    Ok(updated)
}

fn apply_upsert_http_route(
    draft: &ConfigDraft,
    service_name: &str,
    route_value: Value,
    index: Option<usize>,
) -> Result<ConfigDraft, ResourcePatchError> {
    let route: HttpRoute = serde_json::from_value(route_value)
        .map_err(|err| ResourcePatchError::Invalid(format!("invalid HTTP route: {err}")))?;
    let mut updated = draft.clone();
    let service = updated
        .http_services
        .iter_mut()
        .find(|svc| svc.name == service_name)
        .ok_or_else(|| {
            ResourcePatchError::NotFound(format!("HTTP service `{service_name}` was not found"))
        })?;
    match index {
        Some(idx) if idx < service.routes.len() => service.routes[idx] = route,
        Some(idx) => {
            return Err(ResourcePatchError::NotFound(format!(
                "route index {idx} out of bounds for HTTP service `{service_name}`"
            )));
        }
        None => service.routes.push(route),
    }
    Ok(updated)
}

fn apply_remove_http_route(
    draft: &ConfigDraft,
    service_name: &str,
    index: usize,
) -> Result<ConfigDraft, ResourcePatchError> {
    let mut updated = draft.clone();
    let service = updated
        .http_services
        .iter_mut()
        .find(|svc| svc.name == service_name)
        .ok_or_else(|| {
            ResourcePatchError::NotFound(format!("HTTP service `{service_name}` was not found"))
        })?;
    if index >= service.routes.len() {
        return Err(ResourcePatchError::NotFound(format!(
            "route index {index} out of bounds for HTTP service `{service_name}`"
        )));
    }
    service.routes.remove(index);
    Ok(updated)
}

fn draft_to_object(draft: &ConfigDraft) -> Result<Map<String, Value>, ResourcePatchError> {
    match serde_json::to_value(draft) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(ResourcePatchError::Invalid(
            "configuration draft must serialize to a JSON object".to_owned(),
        )),
        Err(err) => Err(ResourcePatchError::Invalid(format!(
            "cannot serialize configuration draft: {err}"
        ))),
    }
}

fn object_to_draft(map: Map<String, Value>) -> Result<ConfigDraft, ResourcePatchError> {
    serde_json::from_value(Value::Object(map))
        .map_err(|err| ResourcePatchError::Invalid(format!("invalid configuration field: {err}")))
}
