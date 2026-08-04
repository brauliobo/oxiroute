use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
};

use rml_rtmp::sessions::ServerSession;

use crate::{vod::parse_flv_events, MediaEvent, VodApplication, VodError, VodLease};

use super::{runtime::ApplicationSessionLease, status::RtmpSessionError};

struct VodState {
    events: VecDeque<MediaEvent>,
    error: Option<VodError>,
    finished: bool,
    completed: bool,
}

pub(super) struct VodPlaybackSession {
    application: String,
    stream_name: String,
    protocol_stream_id: u32,
    state: Arc<Mutex<VodState>>,
    worker: Option<JoinHandle<()>>,
    _vod_lease: VodLease,
    _session_lease: ApplicationSessionLease,
}

pub(super) struct VodPlaybackStart {
    pub(super) application: String,
    pub(super) stream_name: String,
    pub(super) protocol_stream_id: u32,
    pub(super) application_source: Arc<VodApplication>,
    pub(super) source: String,
    pub(super) path: String,
    pub(super) vod_lease: VodLease,
    pub(super) session_lease: ApplicationSessionLease,
}

impl VodPlaybackSession {
    pub(super) fn start(start: VodPlaybackStart) -> Self {
        let state = Arc::new(Mutex::new(VodState {
            events: VecDeque::new(),
            error: None,
            finished: false,
            completed: false,
        }));
        let worker_state = Arc::clone(&state);
        let application_source = start.application_source;
        let worker_source = start.source;
        let worker_path = start.path;
        let worker = thread::Builder::new()
            .name("rtmp-vod-worker".into())
            .spawn(move || {
                let result = application_source
                    .load(&worker_source, &worker_path)
                    .and_then(|(bytes, max_duration)| parse_flv_events(&bytes, max_duration));
                let mut state = worker_state.lock().expect("VOD playback mutex poisoned");
                match result {
                    Ok(events) => state.events.extend(events),
                    Err(error) => state.error = Some(error),
                }
                state.finished = true;
            })
            .ok();
        if worker.is_none() {
            let mut state = state.lock().expect("VOD playback mutex poisoned");
            state.error = Some(VodError::Fetch);
            state.finished = true;
        }
        Self {
            application: start.application,
            stream_name: start.stream_name,
            protocol_stream_id: start.protocol_stream_id,
            state,
            worker,
            _vod_lease: start.vod_lease,
            _session_lease: start.session_lease,
        }
    }

    pub(super) fn matches(&self, application: &str, stream_name: &str) -> bool {
        self.application == application && self.stream_name == stream_name
    }

    pub(super) fn application(&self) -> &str {
        &self.application
    }

    pub(super) fn stream_name(&self) -> &str {
        &self.stream_name
    }

    pub(super) fn release(&mut self) {
        self.join_worker();
    }

    pub(super) fn drain(
        &mut self,
        protocol: &mut ServerSession,
        maximum_events: usize,
    ) -> Result<Vec<Vec<u8>>, RtmpSessionError> {
        let events = {
            let mut state = self.state.lock().expect("VOD playback mutex poisoned");
            let event_count = maximum_events.min(state.events.len());
            state.events.drain(..event_count).collect::<Vec<_>>()
        };
        if !events.is_empty() {
            return events
                .into_iter()
                .map(|event| {
                    super::playback::serialize_event(protocol, self.protocol_stream_id, &event)
                })
                .collect();
        }
        let should_complete = {
            let mut state = self.state.lock().expect("VOD playback mutex poisoned");
            if let Some(error) = state.error.take() {
                return Err(RtmpSessionError::Vod(error));
            }
            if state.finished && !state.completed {
                state.completed = true;
                true
            } else {
                false
            }
        };
        if should_complete {
            self.join_worker();
            return Ok(vec![
                protocol
                    .finish_playing(self.protocol_stream_id)
                    .map_err(RtmpSessionError::from)?
                    .bytes,
            ]);
        }
        Ok(Vec::new())
    }

    fn join_worker(&mut self) {
        if self
            .worker
            .take()
            .is_some_and(|worker| worker.join().is_err())
        {
            let mut state = self.state.lock().expect("VOD playback mutex poisoned");
            state.finished = true;
            state.error = Some(VodError::Fetch);
            state.events.clear();
        }
    }
}

impl Drop for VodPlaybackSession {
    fn drop(&mut self) {
        self.join_worker();
    }
}
