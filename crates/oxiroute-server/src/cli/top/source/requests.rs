use serde::Deserialize;

use super::count::Count;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Requests {
    pub(crate) total: Count,
    pub(crate) truncated: bool,
    #[serde(rename = "requests")]
    pub(crate) items: Vec<Request>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Request {
    #[serde(rename = "requestId")]
    pub(crate) id: String,
    pub(crate) listener: String,
    #[serde(default)]
    pub(crate) route: Option<String>,
    pub(crate) pool: Option<String>,
    pub(crate) server: Option<String>,
    pub(crate) method: String,
    pub(crate) host: Option<String>,
    pub(crate) path: String,
    pub(crate) client_ip: Option<String>,
    pub(crate) phase: String,
    pub(crate) age_ms: Count,
    pub(crate) waited_ms: Option<Count>,
    pub(crate) attempts: u32,
    pub(crate) overdue: bool,
}
