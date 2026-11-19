use serde::Deserialize;

use super::count::Count;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClientsSort {
    Bytes,
    Connections,
    Requests,
    Recent,
}

impl ClientsSort {
    pub(crate) fn for_column(key: &str) -> Self {
        match key {
            "conn" | "conns" | "new" => Self::Connections,
            "req" | "reqs" | "err" | "errors" => Self::Requests,
            "age" | "idle" => Self::Recent,
            _ => Self::Bytes,
        }
    }

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Bytes => "bytes",
            Self::Connections => "connections",
            Self::Requests => "requests",
            Self::Recent => "recent",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ClientsQuery {
    pub(crate) limit: usize,
    pub(crate) sort: ClientsSort,
    pub(crate) ip: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClientsDoc {
    pub(crate) sampled_at_unix_ms: Count,
    #[serde(default)]
    pub(crate) ipv6_prefix: Option<Count>,
    pub(crate) tracked_clients: Count,
    pub(crate) truncated: bool,
    pub(crate) clients: Vec<ClientRow>,
    pub(crate) other: Aggregate,
    pub(crate) totals: Aggregate,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClientRow {
    pub(crate) ip: String,
    pub(crate) connections: Count,
    pub(crate) connections_total: Count,
    pub(crate) requests: Count,
    pub(crate) bytes_in: Count,
    pub(crate) bytes_out: Count,
    pub(crate) first_seen_unix_ms: Count,
    pub(crate) last_activity_unix_ms: Count,
    #[serde(default)]
    pub(crate) listeners: Vec<String>,
    #[serde(default)]
    pub(crate) protocols: Vec<String>,
    #[serde(default)]
    pub(crate) errors: Count,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Aggregate {
    pub(crate) clients: Count,
    pub(crate) connections: Count,
    pub(crate) requests: Count,
    pub(crate) bytes_in: Count,
    pub(crate) bytes_out: Count,
}
