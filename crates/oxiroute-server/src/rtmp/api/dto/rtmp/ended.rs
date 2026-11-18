use oxiroute_rtmp::{EndedStreamSnapshot, PublisherEndReason, StreamProfile};
use schemars::JsonSchema;
use serde::Serialize;

use super::RtmpMediaDto;

#[derive(JsonSchema, Serialize)]
pub(super) struct RtmpProfileDto {
    video_width: Option<u32>,
    video_height: Option<u32>,
    frame_rate_millihertz: Option<u32>,
    video_bitrate_bps: Option<u64>,
    audio_bitrate_bps: Option<u64>,
    keyframe_interval_ms: Option<u32>,
    audio_sample_rate_hz: Option<u32>,
    audio_channels: Option<u8>,
}

impl From<&StreamProfile> for RtmpProfileDto {
    fn from(profile: &StreamProfile) -> Self {
        Self {
            video_width: profile.video_width,
            video_height: profile.video_height,
            frame_rate_millihertz: profile.frame_rate_millihertz,
            video_bitrate_bps: profile.video_bitrate_bps,
            audio_bitrate_bps: profile.audio_bitrate_bps,
            keyframe_interval_ms: profile.keyframe_interval_ms,
            audio_sample_rate_hz: profile.audio_sample_rate_hz,
            audio_channels: profile.audio_channels,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
enum RtmpEndReasonDto {
    Closed,
    Dropped,
    Stale,
    Shutdown,
    Failed,
}

impl From<PublisherEndReason> for RtmpEndReasonDto {
    fn from(reason: PublisherEndReason) -> Self {
        match reason {
            PublisherEndReason::Closed => Self::Closed,
            PublisherEndReason::Dropped => Self::Dropped,
            PublisherEndReason::Stale => Self::Stale,
            PublisherEndReason::Shutdown => Self::Shutdown,
            PublisherEndReason::Failed => Self::Failed,
        }
    }
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct RtmpEndedStreamResponse {
    id: String,
    server_id: String,
    application: String,
    name: String,
    state: &'static str,
    created_at_unix_ms: u64,
    published_at_unix_ms: u64,
    last_seen_at_unix_ms: u64,
    ended_at_unix_ms: u64,
    end_reason: RtmpEndReasonDto,
    publisher_session_id: String,
    media: RtmpMediaDto,
}

impl From<&EndedStreamSnapshot> for RtmpEndedStreamResponse {
    fn from(stream: &EndedStreamSnapshot) -> Self {
        Self {
            id: stream.stream_id.to_string(),
            server_id: stream.key.server_id.clone(),
            application: stream.key.application.clone(),
            name: stream.key.name.clone(),
            state: "offline",
            created_at_unix_ms: stream.created_at_unix_ms,
            published_at_unix_ms: stream.published_at_unix_ms,
            last_seen_at_unix_ms: stream.last_seen_at_unix_ms,
            ended_at_unix_ms: stream.ended_at_unix_ms,
            end_reason: stream.end_reason.into(),
            publisher_session_id: stream.publisher_session_id.to_string(),
            media: (&stream.media).into(),
        }
    }
}
