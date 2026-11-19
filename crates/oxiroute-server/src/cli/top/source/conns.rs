use serde::Deserialize;

use super::count::{Count, Label};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConnsQuery {
    pub(crate) limit: usize,
    pub(crate) ip: Option<String>,
    pub(crate) listener: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnsDoc {
    pub(crate) sampled_at_unix_ms: Count,
    pub(crate) truncated: bool,
    pub(crate) total: Count,
    pub(crate) connections: Vec<Conn>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Conn {
    pub(crate) id: Label,
    pub(crate) peer_ip: String,
    pub(crate) peer_port: Count,
    pub(crate) listener: String,
    pub(crate) protocol: String,
    #[serde(default)]
    pub(crate) tls: Option<Tls>,
    pub(crate) state: String,
    pub(crate) age_ms: Count,
    pub(crate) idle_ms: Count,
    pub(crate) requests: Count,
    pub(crate) bytes_in: Count,
    pub(crate) bytes_out: Count,
    #[serde(default)]
    pub(crate) service: Option<String>,
    #[serde(default)]
    pub(crate) upstream: Option<String>,
    #[serde(default)]
    pub(crate) current_request: Option<CurrentRequest>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Tls {
    #[serde(default)]
    pub(crate) version: Option<Label>,
    #[serde(default)]
    pub(crate) sni: Option<Label>,
    #[serde(default)]
    pub(crate) alpn: Option<Label>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CurrentRequest {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) age_ms: Count,
}
