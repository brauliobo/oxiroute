use oxiroute_rtmp::{
    RecorderPhase, RecorderSnapshot, RelaySnapshot, RtmpCatalogSnapshot, RtmpClientSnapshot,
    StreamSnapshot, TrackSnapshot, VideoCodecIdentifier,
};
use schemars::JsonSchema;
use serde::Serialize;

use super::DecimalCounter;
use crate::rtmp_api::wire::{
    RecorderErrorCodeDto, RecorderNotificationDto, RelayDnsRefreshFailureDto, RelayFailureDto,
    RelayPhaseDto, RtmpSessionControlActionDto, RtmpSessionControlOutcomeDto, RtmpSessionRoleDto,
};

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RtmpStatsResponse {
    revision: String,
    as_of_unix_ms: u64,
    global: RtmpGlobalStatsDto,
    live: Vec<RtmpLiveStreamDto>,
    clients: Vec<RtmpClientDto>,
    live_truncated: bool,
    clients_truncated: bool,
}

impl RtmpStatsResponse {
    pub(crate) fn project(
        snapshot: &RtmpCatalogSnapshot,
        clients: &[RtmpClientSnapshot],
        max_streams: usize,
        max_clients: usize,
    ) -> Self {
        Self {
            revision: snapshot.revision.to_string(),
            as_of_unix_ms: snapshot.as_of_unix_ms,
            global: RtmpGlobalStatsDto::project(snapshot),
            live: live_streams(snapshot, max_streams),
            clients: client_stats(clients, max_clients),
            live_truncated: snapshot.streams.len() > max_streams,
            clients_truncated: clients.len() > max_clients,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RtmpGlobalStatsResponse {
    revision: String,
    as_of_unix_ms: u64,
    global: RtmpGlobalStatsDto,
}

impl RtmpGlobalStatsResponse {
    pub(crate) fn project(snapshot: &RtmpCatalogSnapshot) -> Self {
        Self {
            revision: snapshot.revision.to_string(),
            as_of_unix_ms: snapshot.as_of_unix_ms,
            global: RtmpGlobalStatsDto::project(snapshot),
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RtmpLiveStatsResponse {
    revision: String,
    as_of_unix_ms: u64,
    live: Vec<RtmpLiveStreamDto>,
    truncated: bool,
}

impl RtmpLiveStatsResponse {
    pub(crate) fn project(snapshot: &RtmpCatalogSnapshot, max_streams: usize) -> Self {
        Self {
            revision: snapshot.revision.to_string(),
            as_of_unix_ms: snapshot.as_of_unix_ms,
            live: live_streams(snapshot, max_streams),
            truncated: snapshot.streams.len() > max_streams,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RtmpClientStatsResponse {
    revision: String,
    as_of_unix_ms: u64,
    clients: Vec<RtmpClientDto>,
    truncated: bool,
}

impl RtmpClientStatsResponse {
    pub(crate) fn project(
        snapshot: &RtmpCatalogSnapshot,
        clients: &[RtmpClientSnapshot],
        max_clients: usize,
    ) -> Self {
        Self {
            revision: snapshot.revision.to_string(),
            as_of_unix_ms: snapshot.as_of_unix_ms,
            clients: client_stats(clients, max_clients),
            truncated: clients.len() > max_clients,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct RtmpGlobalStatsDto {
    active_streams: usize,
    publishers: u64,
    subscribers: u64,
    audio_payload_bytes: DecimalCounter,
    video_payload_bytes: DecimalCounter,
    live_ingest: bool,
    manual_recording: bool,
}

impl RtmpGlobalStatsDto {
    fn project(snapshot: &RtmpCatalogSnapshot) -> Self {
        let mut publishers = 0_u64;
        let mut subscribers = 0_u64;
        let mut audio_payload_bytes = 0_u64;
        let mut video_payload_bytes = 0_u64;
        for stream in &snapshot.streams {
            publishers = publishers.saturating_add(u64::from(stream.publisher.is_some()));
            subscribers = subscribers
                .saturating_add(u64::try_from(stream.subscriber_count).unwrap_or(u64::MAX));
            audio_payload_bytes =
                audio_payload_bytes.saturating_add(stream.media.audio.payload_bytes_received);
            video_payload_bytes =
                video_payload_bytes.saturating_add(stream.media.video.payload_bytes_received);
        }
        Self {
            active_streams: snapshot.streams.len(),
            publishers,
            subscribers,
            audio_payload_bytes: audio_payload_bytes.into(),
            video_payload_bytes: video_payload_bytes.into(),
            live_ingest: snapshot.capabilities.live_ingest,
            manual_recording: snapshot.capabilities.manual_recording,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct RtmpLiveStreamDto {
    id: String,
    service: String,
    application: String,
    name: String,
    created_at_unix_ms: u64,
    publisher_session_id: Option<String>,
    subscriber_count: usize,
    audio_payload_bytes: DecimalCounter,
    video_payload_bytes: DecimalCounter,
}

impl From<&StreamSnapshot> for RtmpLiveStreamDto {
    fn from(stream: &StreamSnapshot) -> Self {
        Self {
            id: stream.id.to_string(),
            service: stream.key.server_id.clone(),
            application: stream.key.application.clone(),
            name: stream.key.name.clone(),
            created_at_unix_ms: stream.created_at_unix_ms,
            publisher_session_id: stream
                .publisher
                .map(|publisher| publisher.session_id.to_string()),
            subscriber_count: stream.subscriber_count,
            audio_payload_bytes: stream.media.audio.payload_bytes_received.into(),
            video_payload_bytes: stream.media.video.payload_bytes_received.into(),
        }
    }
}

fn live_streams(snapshot: &RtmpCatalogSnapshot, max_streams: usize) -> Vec<RtmpLiveStreamDto> {
    snapshot
        .streams
        .iter()
        .take(max_streams)
        .map(Into::into)
        .collect()
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct RtmpClientDto {
    id: String,
    service: String,
    peer_ip: Option<String>,
    connected_at_unix_ms: u64,
    application: Option<String>,
    stream: Option<String>,
    role: RtmpSessionRoleDto,
    message_streams: Vec<RtmpMessageStreamDto>,
    revision: String,
}

impl From<&RtmpClientSnapshot> for RtmpClientDto {
    fn from(client: &RtmpClientSnapshot) -> Self {
        Self {
            id: client.session_id.to_string(),
            service: client.service_id.clone(),
            peer_ip: client.peer_addr.map(|address| address.to_string()),
            connected_at_unix_ms: client.connected_at_unix_ms,
            application: client.application.clone(),
            stream: client.stream_name.clone(),
            role: client.role.into(),
            message_streams: client
                .message_streams
                .iter()
                .map(|stream| RtmpMessageStreamDto {
                    message_stream_id: stream.message_stream_id,
                    application: stream.application.clone(),
                    stream: stream.stream_name.clone(),
                    role: stream.role.into(),
                })
                .collect(),
            revision: client.revision.to_string(),
        }
    }
}

fn client_stats(clients: &[RtmpClientSnapshot], max_clients: usize) -> Vec<RtmpClientDto> {
    clients.iter().take(max_clients).map(Into::into).collect()
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct RtmpMessageStreamDto {
    message_stream_id: u32,
    application: String,
    stream: String,
    role: RtmpSessionRoleDto,
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct RtmpCatalogResponse {
    revision: String,
    as_of_unix_ms: u64,
    capabilities: RtmpCapabilitiesDto,
    streams: Vec<RtmpStreamResponse>,
}

impl RtmpCatalogResponse {
    pub(crate) fn project(snapshot: &RtmpCatalogSnapshot) -> Self {
        Self {
            revision: snapshot.revision.to_string(),
            as_of_unix_ms: snapshot.as_of_unix_ms,
            capabilities: RtmpCapabilitiesDto {
                live_ingest: snapshot.capabilities.live_ingest,
                manual_recording: snapshot.capabilities.manual_recording,
            },
            streams: snapshot.streams.iter().map(Into::into).collect(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
struct RtmpCapabilitiesDto {
    live_ingest: bool,
    manual_recording: bool,
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct RtmpStreamResponse {
    id: String,
    revision: String,
    server_id: String,
    application: String,
    name: String,
    created_at_unix_ms: u64,
    publisher: Option<RtmpPublisherDto>,
    subscriber_count: usize,
    media: RtmpMediaDto,
    relays: Vec<RtmpRelayDto>,
    recording_supported: bool,
    manual_recording: bool,
    recorders: Vec<RtmpRecorderResponse>,
}

impl From<&StreamSnapshot> for RtmpStreamResponse {
    fn from(stream: &StreamSnapshot) -> Self {
        Self {
            id: stream.id.to_string(),
            revision: stream.revision.to_string(),
            server_id: stream.key.server_id.clone(),
            application: stream.key.application.clone(),
            name: stream.key.name.clone(),
            created_at_unix_ms: stream.created_at_unix_ms,
            publisher: stream.publisher.map(|publisher| RtmpPublisherDto {
                session_id: publisher.session_id.to_string(),
                attached_at_unix_ms: publisher.attached_at_unix_ms,
            }),
            subscriber_count: stream.subscriber_count,
            media: RtmpMediaDto {
                audio: (&stream.media.audio).into(),
                video: (&stream.media.video).into(),
                fanout_payload_bytes: stream.media.fanout_payload_bytes_queued.into(),
            },
            relays: stream.relays.iter().map(Into::into).collect(),
            recording_supported: !stream.recorders.is_empty(),
            manual_recording: stream.recorders.iter().any(|recorder| recorder.manual),
            recorders: stream.recorders.iter().map(Into::into).collect(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
struct RtmpPublisherDto {
    session_id: String,
    attached_at_unix_ms: u64,
}

#[derive(JsonSchema, Serialize)]
struct RtmpMediaDto {
    audio: RtmpTrackDto,
    video: RtmpTrackDto,
    fanout_payload_bytes: DecimalCounter,
}

#[derive(JsonSchema, Serialize)]
struct RtmpTrackDto {
    codec_id: Option<u8>,
    codec_fourcc: Option<String>,
    codec_name: Option<RtmpCodecNameDto>,
    recording_supported: bool,
    payload_bytes: DecimalCounter,
    last_rtmp_timestamp_ms: Option<u32>,
    last_observed_at_unix_ms: Option<u64>,
}

impl From<&TrackSnapshot> for RtmpTrackDto {
    fn from(track: &TrackSnapshot) -> Self {
        let codec_id = track
            .video_codec
            .and_then(VideoCodecIdentifier::flv_codec_id)
            .or(track.flv_codec_id);
        let codec_fourcc = track.video_codec.and_then(VideoCodecIdentifier::four_cc);
        let codec_name = track
            .video_codec
            .and_then(video_codec_name)
            .or_else(|| codec_id.and_then(flv_codec_name));
        let recording_supported = track.video_codec.map_or_else(
            || matches!(track.flv_codec_id, Some(7 | 10)),
            VideoCodecIdentifier::recording_supported,
        );
        Self {
            codec_id,
            codec_fourcc: codec_fourcc
                .map(|four_cc| String::from_utf8_lossy(&four_cc).into_owned()),
            codec_name,
            recording_supported,
            payload_bytes: track.payload_bytes_received.into(),
            last_rtmp_timestamp_ms: track.last_rtmp_timestamp_ms,
            last_observed_at_unix_ms: track.last_observed_at_unix_ms,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
enum RtmpCodecNameDto {
    Aac,
    Avc,
    Hevc,
    Av1,
}

const fn flv_codec_name(codec_id: u8) -> Option<RtmpCodecNameDto> {
    match codec_id {
        7 => Some(RtmpCodecNameDto::Avc),
        10 => Some(RtmpCodecNameDto::Aac),
        _ => None,
    }
}

fn video_codec_name(codec: VideoCodecIdentifier) -> Option<RtmpCodecNameDto> {
    match codec {
        VideoCodecIdentifier::Flv(codec_id) => flv_codec_name(codec_id),
        VideoCodecIdentifier::FourCc(four_cc) if four_cc == *b"avc1" => Some(RtmpCodecNameDto::Avc),
        VideoCodecIdentifier::FourCc(four_cc) if four_cc == *b"hvc1" => {
            Some(RtmpCodecNameDto::Hevc)
        }
        VideoCodecIdentifier::FourCc(four_cc) if four_cc == *b"av01" => Some(RtmpCodecNameDto::Av1),
        VideoCodecIdentifier::FourCc(_) => None,
    }
}

#[derive(JsonSchema, Serialize)]
struct RtmpRelayDto {
    id: String,
    destination: RtmpRelayDestinationDto,
    phase: RelayPhaseDto,
    last_failure: Option<RelayFailureDto>,
    queue_messages: usize,
    queue_bytes: DecimalCounter,
    connection_attempts: DecimalCounter,
    connections: DecimalCounter,
    reconnects: DecimalCounter,
    dns_refresh_attempts: DecimalCounter,
    dns_refresh_successes: DecimalCounter,
    dns_refresh_failures: DecimalCounter,
    last_dns_refresh_failure: Option<RelayDnsRefreshFailureDto>,
    events_enqueued: DecimalCounter,
    events_sent: DecimalCounter,
    events_dropped: DecimalCounter,
    payload_bytes_sent: DecimalCounter,
}

impl From<&RelaySnapshot> for RtmpRelayDto {
    fn from(relay: &RelaySnapshot) -> Self {
        Self {
            id: relay.id.to_string(),
            destination: RtmpRelayDestinationDto {
                address: relay.status.destination.address.to_string(),
                application: relay.status.destination.application.clone(),
                stream_name: relay.status.destination.stream_name.clone(),
            },
            phase: relay.status.phase.into(),
            last_failure: relay.status.last_failure.map(Into::into),
            queue_messages: relay.status.queue_messages,
            queue_bytes: u64::try_from(relay.status.queue_bytes)
                .unwrap_or(u64::MAX)
                .into(),
            connection_attempts: relay.status.connection_attempts.into(),
            connections: relay.status.connections.into(),
            reconnects: relay.status.reconnects.into(),
            dns_refresh_attempts: relay.status.dns_refresh_attempts.into(),
            dns_refresh_successes: relay.status.dns_refresh_successes.into(),
            dns_refresh_failures: relay.status.dns_refresh_failures.into(),
            last_dns_refresh_failure: relay.status.last_dns_refresh_failure.map(Into::into),
            events_enqueued: relay.status.events_enqueued.into(),
            events_sent: relay.status.events_sent.into(),
            events_dropped: relay.status.events_dropped.into(),
            payload_bytes_sent: relay.status.payload_bytes_sent.into(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
struct RtmpRelayDestinationDto {
    address: String,
    application: String,
    stream_name: String,
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct RtmpRecorderResponse {
    id: String,
    name: Option<String>,
    manual: bool,
    phase: RtmpRecorderPhaseDto,
    changed_at_unix_ms: u64,
    bytes_written: DecimalCounter,
    current_relative_name: Option<String>,
    last_completed_relative_name: Option<String>,
    recoverable_partial_name: Option<String>,
    published_but_not_durable_relative_name: Option<String>,
    segments_started: DecimalCounter,
    segments_completed: DecimalCounter,
    discontinuities: DecimalCounter,
    last_notification: Option<RecorderNotificationDto>,
}

impl From<&RecorderSnapshot> for RtmpRecorderResponse {
    fn from(recorder: &RecorderSnapshot) -> Self {
        Self {
            id: recorder.id.to_string(),
            name: recorder.name.clone(),
            manual: recorder.manual,
            phase: recorder.phase.into(),
            changed_at_unix_ms: recorder.changed_at_unix_ms,
            bytes_written: recorder.bytes_written.into(),
            current_relative_name: recorder.current_relative_name.clone(),
            last_completed_relative_name: recorder.last_completed_relative_name.clone(),
            recoverable_partial_name: recorder.recoverable_partial_name.clone(),
            published_but_not_durable_relative_name: recorder
                .published_but_not_durable_relative_name
                .clone(),
            segments_started: recorder.segments_started.into(),
            segments_completed: recorder.segments_completed.into(),
            discontinuities: recorder.discontinuities.into(),
            last_notification: recorder.last_notification.map(Into::into),
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum RtmpRecorderPhaseDto {
    Idle,
    Starting {
        operation_id: String,
    },
    Recording {
        operation_id: String,
        started_at_unix_ms: u64,
    },
    Stopping {
        operation_id: String,
    },
    Failed {
        operation_id: String,
        code: RecorderErrorCodeDto,
    },
}

impl From<RecorderPhase> for RtmpRecorderPhaseDto {
    fn from(phase: RecorderPhase) -> Self {
        match phase {
            RecorderPhase::Idle => Self::Idle,
            RecorderPhase::Starting { operation_id } => Self::Starting {
                operation_id: operation_id.to_string(),
            },
            RecorderPhase::Recording {
                operation_id,
                started_at_unix_ms,
            } => Self::Recording {
                operation_id: operation_id.to_string(),
                started_at_unix_ms,
            },
            RecorderPhase::Stopping { operation_id } => Self::Stopping {
                operation_id: operation_id.to_string(),
            },
            RecorderPhase::Failed { operation_id, code } => Self::Failed {
                operation_id: operation_id.to_string(),
                code: code.into(),
            },
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RtmpSessionControlResponse {
    outcome: RtmpSessionControlOutcomeDto,
    session_id: String,
    target: RtmpSessionControlActionDto,
    session_revision: String,
}

impl RtmpSessionControlResponse {
    pub(crate) fn project(
        already_requested: bool,
        session_id: &impl ToString,
        target: oxiroute_rtmp::RtmpSessionControlAction,
        session_revision: u64,
    ) -> Self {
        Self {
            outcome: already_requested.into(),
            session_id: session_id.to_string(),
            target: target.into(),
            session_revision: session_revision.to_string(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RtmpSessionRevisionConflictResponse {
    error: RtmpSessionRevisionConflictErrorDto,
    actual_revision: u64,
}

impl RtmpSessionRevisionConflictResponse {
    pub(crate) const fn new(actual_revision: u64) -> Self {
        Self {
            error: RtmpSessionRevisionConflictErrorDto {
                code: RtmpSessionRevisionConflictCodeDto::SessionRevisionConflict,
                message: "RTMP session revision changed",
            },
            actual_revision,
        }
    }
}

#[derive(JsonSchema, Serialize)]
struct RtmpSessionRevisionConflictErrorDto {
    code: RtmpSessionRevisionConflictCodeDto,
    message: &'static str,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
enum RtmpSessionRevisionConflictCodeDto {
    SessionRevisionConflict,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RtmpSessionRoleConflictResponse {
    error: RtmpSessionRoleConflictErrorDto,
    actual_role: RtmpSessionRoleDto,
}

impl RtmpSessionRoleConflictResponse {
    pub(crate) fn new(actual_role: oxiroute_rtmp::RtmpSessionRole) -> Self {
        Self {
            error: RtmpSessionRoleConflictErrorDto {
                code: RtmpSessionRoleConflictCodeDto::SessionRoleConflict,
                message: "RTMP session role does not match the requested target",
            },
            actual_role: actual_role.into(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
struct RtmpSessionRoleConflictErrorDto {
    code: RtmpSessionRoleConflictCodeDto,
    message: &'static str,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
enum RtmpSessionRoleConflictCodeDto {
    SessionRoleConflict,
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, str::FromStr};

    use oxiroute_rtmp::{
        MediaSnapshot, RtmpCapabilities, RtmpCatalogSnapshot, RtmpSessionControlAction,
        RtmpSessionRole, SessionId, StreamId, StreamKey, StreamSnapshot, TrackSnapshot,
    };
    use schemars::generate::SchemaSettings;
    use serde_json::{Value, json};

    use super::*;

    fn schema<T: JsonSchema>() -> Value {
        let generator = SchemaSettings::draft2020_12().into_generator();
        serde_json::to_value(generator.into_root_schema_for::<T>()).expect("response schema")
    }

    fn empty_catalog() -> RtmpCatalogSnapshot {
        RtmpCatalogSnapshot {
            revision: 7,
            as_of_unix_ms: 1_750_000_000_000,
            capabilities: RtmpCapabilities {
                live_ingest: true,
                manual_recording: false,
            },
            streams: Vec::new(),
        }
    }

    #[test]
    fn empty_stats_and_catalog_match_the_existing_v1_golden_objects() {
        let catalog = empty_catalog();
        assert_eq!(
            serde_json::to_value(RtmpStatsResponse::project(&catalog, &[], 1_024, 1_024))
                .expect("stats JSON"),
            json!({
                "revision": "7",
                "asOfUnixMs": 1_750_000_000_000_u64,
                "global": {
                    "activeStreams": 0,
                    "publishers": 0,
                    "subscribers": 0,
                    "audioPayloadBytes": "0",
                    "videoPayloadBytes": "0",
                    "liveIngest": true,
                    "manualRecording": false,
                },
                "live": [],
                "clients": [],
                "liveTruncated": false,
                "clientsTruncated": false,
            })
        );
        assert_eq!(
            serde_json::to_value(RtmpCatalogResponse::project(&catalog)).expect("catalog JSON"),
            json!({
                "revision": "7",
                "as_of_unix_ms": 1_750_000_000_000_u64,
                "capabilities": { "live_ingest": true, "manual_recording": false },
                "streams": [],
            })
        );
    }

    #[test]
    fn session_control_success_and_conflicts_match_the_v1_golden_objects() {
        let session_id = SessionId::new();
        assert_eq!(
            serde_json::to_value(RtmpSessionControlResponse::project(
                false,
                &session_id,
                RtmpSessionControlAction::Publisher,
                u64::MAX,
            ))
            .expect("control response JSON"),
            json!({
                "outcome": "requested",
                "sessionId": session_id.to_string(),
                "target": "publisher",
                "sessionRevision": u64::MAX.to_string(),
            })
        );
        assert_eq!(
            serde_json::to_value(RtmpSessionRevisionConflictResponse::new(9))
                .expect("revision conflict JSON"),
            json!({
                "error": {
                    "code": "session_revision_conflict",
                    "message": "RTMP session revision changed",
                },
                "actualRevision": 9,
            })
        );
        assert_eq!(
            serde_json::to_value(RtmpSessionRoleConflictResponse::new(
                RtmpSessionRole::Subscriber,
            ))
            .expect("role conflict JSON"),
            json!({
                "error": {
                    "code": "session_role_conflict",
                    "message": "RTMP session role does not match the requested target",
                },
                "actualRole": "subscriber",
            })
        );
    }

    #[test]
    fn stats_and_catalog_preserve_maximum_cumulative_counters_as_decimal_strings() {
        let mut catalog = empty_catalog();
        catalog.streams.push(StreamSnapshot {
            id: StreamId::from_str("2a130dea-5db7-43e0-afb8-f07c4bcb1814")
                .expect("fixed stream ID"),
            revision: u64::MAX,
            key: StreamKey::new("edge", "live", "camera"),
            created_at_unix_ms: 1_750_000_000_000,
            publisher: None,
            subscriber_count: 0,
            media: MediaSnapshot {
                audio: TrackSnapshot {
                    payload_bytes_received: u64::MAX,
                    ..TrackSnapshot::default()
                },
                video: TrackSnapshot::default(),
                fanout_payload_bytes_queued: u64::MAX,
            },
            relays: Vec::new(),
            recorders: Vec::new(),
        });

        let stats = serde_json::to_value(RtmpStatsResponse::project(&catalog, &[], 1, 1))
            .expect("stats JSON");
        let catalog =
            serde_json::to_value(RtmpCatalogResponse::project(&catalog)).expect("catalog JSON");

        assert_eq!(stats["global"]["audioPayloadBytes"], u64::MAX.to_string());
        assert_eq!(stats["live"][0]["audioPayloadBytes"], u64::MAX.to_string());
        assert_eq!(catalog["streams"][0]["revision"], u64::MAX.to_string());
        assert_eq!(
            catalog["streams"][0]["media"]["audio"]["payload_bytes"],
            u64::MAX.to_string()
        );
        assert_eq!(
            catalog["streams"][0]["media"]["fanout_payload_bytes"],
            u64::MAX.to_string()
        );
    }

    #[test]
    fn rtmp_response_schemas_are_closed_structural_projections_with_decimal_counters() {
        let schemas = [
            schema::<RtmpStatsResponse>(),
            schema::<RtmpGlobalStatsResponse>(),
            schema::<RtmpLiveStatsResponse>(),
            schema::<RtmpClientStatsResponse>(),
            schema::<RtmpCatalogResponse>(),
            schema::<RtmpStreamResponse>(),
            schema::<RtmpRecorderResponse>(),
            schema::<RtmpSessionControlResponse>(),
            schema::<RtmpSessionRevisionConflictResponse>(),
            schema::<RtmpSessionRoleConflictResponse>(),
        ];
        for response in &schemas {
            let encoded = response.to_string();
            assert!(!encoded.contains("serde_json::Value"));
            assert!(!encoded.contains("\"additionalProperties\":true"));
        }
        let catalog = schema::<RtmpCatalogResponse>();
        assert_eq!(catalog["$defs"]["DecimalCounter"]["type"], "string");
        assert_eq!(
            catalog["$defs"]["RtmpRelayDto"]["properties"]["payload_bytes_sent"]["$ref"],
            "#/$defs/DecimalCounter"
        );
        assert_eq!(
            catalog["$defs"]["RtmpRecorderResponse"]["properties"]["bytes_written"]["$ref"],
            "#/$defs/DecimalCounter"
        );
    }

    #[test]
    fn rtmp_schemas_own_the_expected_nested_sections_and_exclude_secret_fields() {
        let catalog_schema = schema::<RtmpCatalogResponse>();
        let definitions = catalog_schema["$defs"]
            .as_object()
            .expect("catalog schema definitions")
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        for expected in [
            "RtmpPublisherDto",
            "RtmpTrackDto",
            "RtmpRelayDto",
            "RtmpRecorderResponse",
        ] {
            assert!(definitions.contains(expected), "missing {expected}");
        }
        let schemas = [
            schema::<RtmpStatsResponse>(),
            schema::<RtmpCatalogResponse>(),
            schema::<RtmpSessionControlResponse>(),
        ]
        .into_iter()
        .map(|schema| schema.to_string().to_ascii_lowercase())
        .collect::<String>();
        for forbidden in ["token", "password", "secret", "authorization", "privatekey"] {
            assert!(!schemas.contains(forbidden), "schema exposed {forbidden}");
        }
    }
}
