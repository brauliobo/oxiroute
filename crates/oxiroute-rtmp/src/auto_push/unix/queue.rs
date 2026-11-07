#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) struct PeerQueue {
    max_messages: usize,
    max_bytes: usize,
    state: Mutex<PeerQueueState>,
    wake: Condvar,
}

pub(super) struct PeerQueueState {
    accepting: bool,
    waiting_for_bootstrap: bool,
    pub(super) queue: VecDeque<AutoPushFrame>,
    pub(super) bytes: usize,
}

#[derive(Clone)]
pub(super) struct AutoPushFrame {
    pub(super) application: String,
    pub(super) stream_name: String,
    pub(super) token: SourceToken,
    pub(super) sequence: u64,
    pub(super) event: MediaEvent,
}

#[derive(Clone, Default)]
pub(super) struct AutoPushCache {
    metadata: Option<AutoPushFrame>,
    aac_header: Option<AutoPushFrame>,
    video_headers: BTreeMap<VideoCodec, AutoPushFrame>,
    keyframe: Option<AutoPushFrame>,
    latest_audio: Option<AutoPushFrame>,
}

impl PeerQueue {
    pub(super) fn new(config: &RtmpAutoPushConfig) -> Arc<Self> {
        Arc::new(Self {
            max_messages: config.max_queue_messages,
            max_bytes: config.max_queue_bytes,
            state: Mutex::new(PeerQueueState {
                accepting: true,
                waiting_for_bootstrap: false,
                queue: VecDeque::new(),
                bytes: 0,
            }),
            wake: Condvar::new(),
        })
    }

    pub(super) fn enqueue(&self, frames: &[AutoPushFrame], bootstrap: bool) -> bool {
        let mut state = self.lock();
        if !state.accepting || (state.waiting_for_bootstrap && !bootstrap) {
            return false;
        }
        let frame_bytes = frames
            .iter()
            .try_fold(0_usize, |total, frame| total.checked_add(frame.size()));
        let Some(frame_bytes) = frame_bytes else {
            state.waiting_for_bootstrap = true;
            Self::clear(&mut state);
            return false;
        };
        let Some(message_count) = state.queue.len().checked_add(frames.len()) else {
            state.waiting_for_bootstrap = true;
            Self::clear(&mut state);
            return false;
        };
        let Some(queue_bytes) = state.bytes.checked_add(frame_bytes) else {
            state.waiting_for_bootstrap = true;
            Self::clear(&mut state);
            return false;
        };
        if message_count > self.max_messages || queue_bytes > self.max_bytes {
            Self::clear(&mut state);
            state.waiting_for_bootstrap = true;
            if !bootstrap {
                return false;
            }
            if frames.len() > self.max_messages || frame_bytes > self.max_bytes {
                return false;
            }
        }
        state.queue.extend(frames.iter().cloned());
        state.bytes = state.bytes.saturating_add(frame_bytes);
        state.waiting_for_bootstrap = false;
        drop(state);
        self.wake.notify_one();
        true
    }

    pub(super) fn next(&self) -> Option<AutoPushFrame> {
        let mut state = self.lock();
        while state.accepting && state.queue.is_empty() {
            state = self
                .wake
                .wait_timeout(state, super::SOCKET_WAIT)
                .expect("RTMP auto-push queue mutex poisoned")
                .0;
        }
        let frame = state.queue.pop_front()?;
        state.bytes = state.bytes.saturating_sub(frame.size());
        Some(frame)
    }

    pub(super) fn close(&self) {
        let mut state = self.lock();
        state.accepting = false;
        Self::clear(&mut state);
        drop(state);
        self.wake.notify_all();
    }

    pub(super) fn lock(&self) -> std::sync::MutexGuard<'_, PeerQueueState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn clear(state: &mut PeerQueueState) {
        state.queue.clear();
        state.bytes = 0;
    }
}

impl AutoPushFrame {
    pub(super) fn size(&self) -> usize {
        self.application.len()
            + self.stream_name.len()
            + self.token.session_id.len()
            + self.event.payload_len()
            + 64
    }

    pub(super) fn is_keyframe(&self) -> bool {
        self.event.kind() == MediaEventKind::VideoKeyframe
    }
}

impl AutoPushCache {
    pub(super) fn update(&mut self, frame: AutoPushFrame) {
        match frame.event.kind() {
            MediaEventKind::Metadata => self.metadata = Some(frame),
            MediaEventKind::AacSequenceHeader => self.aac_header = Some(frame),
            MediaEventKind::AvcSequenceHeader
            | MediaEventKind::HevcSequenceHeader
            | MediaEventKind::Av1SequenceHeader => {
                if let Some(codec) = frame.event.video_codec() {
                    self.keyframe = None;
                    self.video_headers.insert(codec, frame);
                }
            }
            MediaEventKind::VideoKeyframe => self.keyframe = Some(frame),
            MediaEventKind::Audio => self.latest_audio = Some(frame),
            MediaEventKind::VideoInterframe | MediaEventKind::VideoDisposable => {}
        }
    }

    pub(super) fn bootstrap(&self) -> Vec<AutoPushFrame> {
        let mut frames = Vec::with_capacity(4);
        frames.extend(self.metadata.iter().cloned());
        frames.extend(self.aac_header.iter().cloned());
        if let Some(keyframe) = &self.keyframe {
            if let Some(codec) = keyframe.event.video_codec() {
                frames.extend(self.video_headers.get(&codec).cloned());
            }
            frames.push(keyframe.clone());
        } else {
            frames.extend(self.latest_audio.iter().cloned());
        }
        frames.sort_by_key(|frame| frame.sequence);
        frames
    }
}

impl RemoteStream {
    pub(super) fn shutdown(mut self, at_unix_ms: u64) {
        let _transaction = self.hub.lock_roles();
        let _ = self.registration.release(at_unix_ms);
        drop(self.lease);
    }
}

pub(super) fn apply_remote_frame(
    remote: &mut RemoteStream,
    frame: &AutoPushFrame,
    at_unix_ms: u64,
) -> Result<(), ()> {
    if frame.sequence <= remote.sequence || frame.token != remote.token {
        return Err(());
    }
    remote.lease.publish(frame.event.clone()).map_err(|_| ())?;
    remote.sequence = frame.sequence;
    remote.media.observe(&frame.event, at_unix_ms);
    remote.registration.observe_at(at_unix_ms);
    remote
        .registry
        .update_media_sample(
            remote.stream_id,
            remote.session_id,
            remote.sequence,
            remote.media.snapshot(0),
            at_unix_ms,
        )
        .map_err(|_| ())?;
    Ok(())
}
