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
mod tests {
    use std::sync::Arc;

    use rml_rtmp::sessions::StreamMetadata;

    use super::*;

    #[test]
    fn identical_traces_produce_identical_controlled_observations() {
        let mut metadata = StreamMetadata::new();
        metadata.audio_codec_id = Some(10);
        metadata.video_codec_id = Some(7);
        let trace = [
            (MediaEvent::metadata(metadata).expect("metadata"), 100),
            (audio(30, &[0xaf, 0, 1]), 110),
            (audio(20, &[0x2f, 1]), 120),
            (video(40, &[0x17, 0, 0, 0, 0, 1]), 130),
            (video(35, &[0x91, b'h', b'v', b'c', b'1', 1]), 140),
            (video(25, &[0x93, b'a', b'v', b'0', b'1', 1]), 150),
        ];
        let mut first = MediaSnapshotAccumulator::default();
        let mut second = MediaSnapshotAccumulator::default();

        for (event, at_unix_ms) in &trace {
            first.observe(event, *at_unix_ms);
            second.observe(event, *at_unix_ms);
        }

        let expected = MediaSnapshot {
            audio: TrackSnapshot {
                flv_codec_id: Some(2),
                payload_bytes_received: 5,
                last_rtmp_timestamp_ms: Some(20),
                last_observed_at_unix_ms: Some(120),
                ..TrackSnapshot::default()
            },
            video: TrackSnapshot {
                flv_codec_id: None,
                video_codec: Some(VideoCodecIdentifier::FourCc(*b"av01")),
                payload_bytes_received: 18,
                last_rtmp_timestamp_ms: Some(25),
                last_observed_at_unix_ms: Some(150),
            },
            fanout_payload_bytes_queued: 77,
        };
        assert_eq!(first, second);
        assert_eq!(first.snapshot(77), expected);
    }

    #[test]
    fn metadata_clears_advertised_codecs_without_rewriting_media_observations() {
        let mut metadata = StreamMetadata::new();
        metadata.audio_codec_id = Some(10);
        metadata.video_codec_id = Some(7);
        let mut accumulator = MediaSnapshotAccumulator::default();
        accumulator.observe(&MediaEvent::metadata(metadata).expect("metadata"), 10);
        accumulator.observe(&audio(9, &[0xaf, 1, 1]), 20);
        accumulator.observe(&video(8, &[0x17, 1, 0, 0, 0, 1]), 30);

        accumulator.observe(
            &MediaEvent::metadata(StreamMetadata::new()).expect("empty metadata"),
            40,
        );

        let snapshot = accumulator.snapshot(0);
        assert_eq!(snapshot.audio.flv_codec_id, None);
        assert_eq!(snapshot.video.flv_codec_id, None);
        assert_eq!(snapshot.video.video_codec, None);
        assert_eq!(snapshot.audio.last_observed_at_unix_ms, Some(20));
        assert_eq!(snapshot.video.last_observed_at_unix_ms, Some(30));
    }

    #[test]
    fn payload_byte_totals_saturate() {
        let mut accumulator = MediaSnapshotAccumulator {
            audio: TrackSnapshot {
                payload_bytes_received: u64::MAX - 1,
                ..TrackSnapshot::default()
            },
            video: TrackSnapshot {
                payload_bytes_received: u64::MAX - 1,
                ..TrackSnapshot::default()
            },
        };

        accumulator.observe(&audio(1, &[0xaf, 1, 1]), 2);
        accumulator.observe(&video(3, &[0x17, 1, 0, 0, 0, 1]), 4);

        let snapshot = accumulator.snapshot(0);
        assert_eq!(snapshot.audio.payload_bytes_received, u64::MAX);
        assert_eq!(snapshot.video.payload_bytes_received, u64::MAX);
    }

    fn audio(timestamp_ms: u32, payload: &[u8]) -> MediaEvent {
        MediaEvent::audio(timestamp_ms, Arc::<[u8]>::from(payload)).expect("audio event")
    }

    fn video(timestamp_ms: u32, payload: &[u8]) -> MediaEvent {
        MediaEvent::video(timestamp_ms, Arc::<[u8]>::from(payload)).expect("video event")
    }
}
