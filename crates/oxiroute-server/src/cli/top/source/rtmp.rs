use serde::Deserialize;

use super::count::Count;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RtmpDoc {
    pub(crate) as_of_unix_ms: Count,
    pub(crate) global: Global,
    pub(crate) live: Vec<Stream>,
    pub(crate) clients: Vec<Session>,
    pub(crate) live_truncated: bool,
    pub(crate) clients_truncated: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Global {
    pub(crate) active_streams: Count,
    pub(crate) publishers: Count,
    pub(crate) subscribers: Count,
    pub(crate) audio_payload_bytes: Count,
    pub(crate) video_payload_bytes: Count,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Stream {
    pub(crate) id: String,
    pub(crate) service: String,
    pub(crate) application: String,
    pub(crate) name: String,
    pub(crate) created_at_unix_ms: Count,
    pub(crate) subscriber_count: Count,
    pub(crate) audio_payload_bytes: Count,
    pub(crate) video_payload_bytes: Count,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Session {
    pub(crate) id: String,
    pub(crate) service: String,
    pub(crate) peer_ip: Option<String>,
    pub(crate) connected_at_unix_ms: Count,
    pub(crate) application: Option<String>,
    pub(crate) stream: Option<String>,
    pub(crate) role: String,
}
