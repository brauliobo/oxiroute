use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MediaTicketRequest {
    #[serde(default)]
    #[schemars(range(min = 1, max = 600))]
    pub(crate) ttl_seconds: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct VodTicketRequest {
    pub(crate) path: String,
    #[serde(default)]
    #[schemars(range(min = 1, max = 600))]
    pub(crate) ttl_seconds: Option<u32>,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TicketResponse {
    pub(crate) ticket: String,
    pub(crate) path: String,
    pub(crate) expires_at_unix_ms: u64,
    pub(crate) ttl_seconds: u32,
}
