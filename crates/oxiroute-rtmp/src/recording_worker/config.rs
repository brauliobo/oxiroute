#[allow(clippy::wildcard_imports)]
use super::*;

/// Queue and keyframe-aligned rotation bounds for one recorder worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecorderWorkerConfig {
    pub max_queue_messages: usize,
    pub max_queue_bytes: usize,
    pub rotation_interval: Option<Duration>,
    pub shutdown_timeout: Duration,
    pub video_codec: Option<RecorderVideoCodec>,
    pub record_mask: RecorderMediaMask,
    pub append: bool,
    pub lock: bool,
    pub max_size: Option<u64>,
    pub max_frames: Option<u64>,
    pub notify: bool,
}

/// Exact track selection for one recorder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecorderMediaMask {
    pub audio: bool,
    pub video: bool,
    pub keyframes: bool,
}

impl Default for RecorderMediaMask {
    fn default() -> Self {
        Self {
            audio: true,
            video: true,
            keyframes: false,
        }
    }
}

impl RecorderMediaMask {
    #[must_use]
    pub const fn new(audio: bool, video: bool, keyframes: bool) -> Self {
        Self {
            audio,
            video,
            keyframes,
        }
    }

    #[must_use]
    pub const fn accepts(self, kind: MediaEventKind) -> bool {
        match kind {
            MediaEventKind::Metadata => self.audio || self.video,
            MediaEventKind::AacSequenceHeader | MediaEventKind::Audio => self.audio,
            MediaEventKind::AvcSequenceHeader
            | MediaEventKind::HevcSequenceHeader
            | MediaEventKind::Av1SequenceHeader
            | MediaEventKind::VideoKeyframe => self.video,
            MediaEventKind::VideoInterframe | MediaEventKind::VideoDisposable => {
                self.video && !self.keyframes
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecorderVideoCodec {
    LegacyAvc,
    EnhancedAvc,
    Hevc,
    Av1,
}

impl Default for RecorderWorkerConfig {
    fn default() -> Self {
        Self {
            max_queue_messages: 256,
            max_queue_bytes: 8 * 1024 * 1024,
            rotation_interval: None,
            shutdown_timeout: Duration::from_secs(5),
            video_codec: None,
            record_mask: RecorderMediaMask::default(),
            append: false,
            lock: false,
            max_size: None,
            max_frames: None,
            notify: false,
        }
    }
}

impl RecorderWorkerConfig {
    /// Validates value-only recorder bounds without starting a worker or touching storage.
    ///
    /// # Errors
    ///
    /// Returns the existing recorder start error for an invalid queue, timeout, track mask,
    /// per-segment limit, or rotation interval.
    pub fn validate_intrinsic(&self) -> Result<(), RecorderWorkerStartError> {
        if self.max_queue_messages == 0 || self.max_queue_bytes == 0 {
            return Err(RecorderWorkerStartError::InvalidQueueLimits);
        }
        if self.shutdown_timeout.is_zero() {
            return Err(RecorderWorkerStartError::InvalidShutdownTimeout);
        }
        if (self.record_mask.keyframes || !self.record_mask.audio) && !self.record_mask.video {
            return Err(RecorderWorkerStartError::InvalidRecordMask);
        }
        if self.max_size == Some(0) || self.max_frames == Some(0) {
            return Err(RecorderWorkerStartError::InvalidRecordingLimit);
        }
        if self.rotation_interval.is_some_and(|interval| {
            interval.is_zero() || interval.as_millis() > MAX_ROTATION_INTERVAL_MS
        }) {
            return Err(RecorderWorkerStartError::InvalidRotationInterval);
        }
        Ok(())
    }
}
