use crate::{MediaEvent, MediaEventKind, MediaSnapshot, TrackSnapshot, VideoCodecIdentifier};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct MediaSnapshotAccumulator {
    audio: TrackSnapshot,
    video: TrackSnapshot,
}

impl MediaSnapshotAccumulator {
    pub(crate) fn observe(&mut self, event: &MediaEvent, at_unix_ms: u64) {
        match event.kind() {
            MediaEventKind::Metadata => {
                let Some(metadata) = event.stream_metadata() else {
                    return;
                };
                self.audio.flv_codec_id = metadata
                    .audio_codec_id
                    .and_then(|codec| u8::try_from(codec).ok());
                self.video.flv_codec_id = metadata
                    .video_codec_id
                    .and_then(|codec| u8::try_from(codec).ok());
                self.video.video_codec = self.video.flv_codec_id.map(VideoCodecIdentifier::Flv);
            }
            MediaEventKind::AacSequenceHeader | MediaEventKind::Audio => {
                self.audio.flv_codec_id = event.payload().first().map(|byte| byte >> 4);
                observe_payload(&mut self.audio, event, at_unix_ms);
            }
            MediaEventKind::AvcSequenceHeader
            | MediaEventKind::HevcSequenceHeader
            | MediaEventKind::Av1SequenceHeader
            | MediaEventKind::VideoKeyframe
            | MediaEventKind::VideoInterframe
            | MediaEventKind::VideoDisposable => {
                self.video.video_codec = event.video_codec_identifier();
                self.video.flv_codec_id = self
                    .video
                    .video_codec
                    .and_then(VideoCodecIdentifier::flv_codec_id);
                observe_payload(&mut self.video, event, at_unix_ms);
            }
        }
    }

    pub(crate) const fn snapshot(self, fanout_payload_bytes_queued: u64) -> MediaSnapshot {
        MediaSnapshot {
            audio: self.audio,
            video: self.video,
            fanout_payload_bytes_queued,
        }
    }
}

fn observe_payload(track: &mut TrackSnapshot, event: &MediaEvent, at_unix_ms: u64) {
    track.payload_bytes_received = track
        .payload_bytes_received
        .saturating_add(event.payload_len() as u64);
    track.last_rtmp_timestamp_ms = Some(event.timestamp_ms());
    track.last_observed_at_unix_ms = Some(at_unix_ms);
}

#[cfg(test)]
mod tests;
