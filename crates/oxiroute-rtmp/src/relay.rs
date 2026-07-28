use std::{
    collections::{BTreeMap, VecDeque},
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, OnceLock,
        mpsc::{self, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

use bytes::Bytes;
use rml_rtmp::{
    handshake::{Handshake, HandshakeProcessResult, PeerType},
    sessions::{
        ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult,
        PublishRequestType,
    },
    time::RtmpTimestamp,
};

use crate::{MediaEvent, MediaEventKind, VideoCodec};

const HANDSHAKE_RESPONSE_BYTES: usize = 3_073;
const RELAY_READ_BUFFER_BYTES: usize = 16 * 1_024;
const RELAY_QUEUE_POLL: Duration = Duration::from_millis(20);
pub const RTMP_RELAY_WORKER_THREADS: usize = 8;
pub const MAX_QUEUED_RTMP_RELAYS: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpDestination {
    pub address: SocketAddr,
    pub application: String,
    pub stream_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpPushTarget {
    pub address: SocketAddr,
    pub application: RtmpPushApplication,
    pub config: RtmpRelayConfig,
}

impl RtmpPushTarget {
    #[must_use]
    pub fn expand(&self, stream_name: &str) -> RtmpDestination {
        RtmpDestination {
            address: self.address,
            application: match &self.application {
                RtmpPushApplication::Exact(application) => application.clone(),
                RtmpPushApplication::StreamName => stream_name.to_owned(),
            },
            stream_name: stream_name.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RtmpPushApplication {
    Exact(String),
    StreamName,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RtmpRelayConfig {
    pub max_queue_messages: usize,
    pub max_queue_bytes: usize,
    pub connect_timeout: Duration,
    pub handshake_timeout: Duration,
    pub reconnect_interval: Duration,
}

impl Default for RtmpRelayConfig {
    fn default() -> Self {
        Self {
            max_queue_messages: 256,
            max_queue_bytes: 8 * 1_024 * 1_024,
            connect_timeout: Duration::from_millis(500),
            handshake_timeout: Duration::from_secs(2),
            reconnect_interval: Duration::from_secs(3),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RtmpRelayPhase {
    Connecting,
    Publishing,
    Backoff,
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RtmpRelayFailure {
    Connect,
    Handshake,
    Session,
    Transport,
    Thread,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpRelayStatus {
    pub destination: RtmpDestination,
    pub phase: RtmpRelayPhase,
    pub last_failure: Option<RtmpRelayFailure>,
    pub queue_messages: usize,
    pub queue_bytes: usize,
    pub connection_attempts: u64,
    pub connections: u64,
    pub reconnects: u64,
    pub events_enqueued: u64,
    pub events_sent: u64,
    pub events_dropped: u64,
    pub payload_bytes_sent: u64,
}

pub(crate) struct RtmpRelayController {
    shared: Arc<RelayShared>,
}

struct RelayShared {
    destination: RtmpDestination,
    config: RtmpRelayConfig,
    state: Mutex<RelayState>,
    available: Condvar,
}

struct RelayState {
    accepting: bool,
    waiting_for_keyframe: bool,
    queue: VecDeque<MediaEvent>,
    queue_bytes: usize,
    cache: RelayCache,
    phase: RtmpRelayPhase,
    last_failure: Option<RtmpRelayFailure>,
    connection_attempts: u64,
    connections: u64,
    reconnects: u64,
    events_enqueued: u64,
    events_sent: u64,
    events_dropped: u64,
    payload_bytes_sent: u64,
}

#[derive(Default)]
struct RelayCache {
    metadata: Option<MediaEvent>,
    aac_header: Option<MediaEvent>,
    video_headers: BTreeMap<VideoCodec, MediaEvent>,
    keyframe: Option<MediaEvent>,
    latest_audio: Option<MediaEvent>,
}

impl RtmpRelayController {
    pub(crate) fn start(destination: RtmpDestination, config: RtmpRelayConfig) -> Arc<Self> {
        let shared = Arc::new(RelayShared {
            destination,
            config,
            state: Mutex::new(RelayState {
                accepting: true,
                waiting_for_keyframe: false,
                queue: VecDeque::new(),
                queue_bytes: 0,
                cache: RelayCache::default(),
                phase: RtmpRelayPhase::Connecting,
                last_failure: None,
                connection_attempts: 0,
                connections: 0,
                reconnects: 0,
                events_enqueued: 0,
                events_sent: 0,
                events_dropped: 0,
                payload_bytes_sent: 0,
            }),
            available: Condvar::new(),
        });
        let controller = Arc::new(Self { shared });
        if relay_executor()
            .admit(Arc::clone(&controller.shared))
            .is_err()
        {
            let mut state = controller.shared.lock();
            state.accepting = false;
            state.phase = RtmpRelayPhase::Stopped;
            state.last_failure = Some(RtmpRelayFailure::Thread);
        }
        controller
    }

    pub(crate) fn try_enqueue(&self, event: MediaEvent) {
        let mut state = self.shared.lock();
        if !state.accepting {
            return;
        }
        let codec_header_changed = state.cache.update(&event);
        if codec_header_changed {
            let discarded = u64::try_from(state.queue.len()).unwrap_or(u64::MAX);
            state.events_dropped = state.events_dropped.saturating_add(discarded);
            state.queue.clear();
            state.queue_bytes = 0;
            state.waiting_for_keyframe = true;
        }
        if state.waiting_for_keyframe {
            if event.kind() != MediaEventKind::VideoKeyframe {
                state.events_dropped = state.events_dropped.saturating_add(1);
                return;
            }
            let bootstrap = state.cache.bootstrap();
            if fits_queue(&bootstrap, self.shared.config) {
                for cached in bootstrap {
                    state.queue_bytes += cached.payload_len();
                    state.queue.push_back(cached);
                }
                state.waiting_for_keyframe = false;
                drop(state);
                self.shared.available.notify_one();
                return;
            }
            state.events_dropped = state.events_dropped.saturating_add(1);
            return;
        }
        let exceeds_messages = state.queue.len() >= self.shared.config.max_queue_messages;
        let queue_bytes = state.queue_bytes.checked_add(event.payload_len());
        if exceeds_messages
            || queue_bytes.is_none_or(|bytes| bytes > self.shared.config.max_queue_bytes)
        {
            let discarded = u64::try_from(state.queue.len()).unwrap_or(u64::MAX);
            state.events_dropped = state
                .events_dropped
                .saturating_add(discarded)
                .saturating_add(1);
            state.queue.clear();
            state.queue_bytes = 0;
            state.waiting_for_keyframe = !state.cache.video_headers.is_empty();
            if event.kind() == MediaEventKind::VideoKeyframe {
                let bootstrap = state.cache.bootstrap();
                if fits_queue(&bootstrap, self.shared.config) {
                    for cached in bootstrap {
                        state.queue_bytes += cached.payload_len();
                        state.queue.push_back(cached);
                    }
                    state.waiting_for_keyframe = false;
                }
            }
            self.shared.available.notify_one();
            return;
        }

        state.queue_bytes = queue_bytes.expect("bounded relay queue byte sum was checked");
        state.queue.push_back(event);
        state.events_enqueued = state.events_enqueued.saturating_add(1);
        drop(state);
        self.shared.available.notify_one();
    }

    pub(crate) fn status(&self) -> RtmpRelayStatus {
        let state = self.shared.lock();
        RtmpRelayStatus {
            destination: self.shared.destination.clone(),
            phase: state.phase,
            last_failure: state.last_failure,
            queue_messages: state.queue.len(),
            queue_bytes: state.queue_bytes,
            connection_attempts: state.connection_attempts,
            connections: state.connections,
            reconnects: state.reconnects,
            events_enqueued: state.events_enqueued,
            events_sent: state.events_sent,
            events_dropped: state.events_dropped,
            payload_bytes_sent: state.payload_bytes_sent,
        }
    }

    pub(crate) fn deactivate(&self) {
        {
            let mut state = self.shared.lock();
            state.accepting = false;
            let discarded = u64::try_from(state.queue.len()).unwrap_or(u64::MAX);
            state.events_dropped = state.events_dropped.saturating_add(discarded);
            state.queue.clear();
            state.queue_bytes = 0;
        }
        self.shared.available.notify_all();
    }
}

impl Drop for RtmpRelayController {
    fn drop(&mut self) {
        self.deactivate();
    }
}

impl RelayShared {
    fn lock(&self) -> MutexGuard<'_, RelayState> {
        self.state.lock().expect("RTMP relay state mutex poisoned")
    }

    fn is_accepting(&self) -> bool {
        self.lock().accepting
    }

    fn set_phase(&self, phase: RtmpRelayPhase) {
        self.lock().phase = phase;
    }

    fn record_failure(&self, failure: RtmpRelayFailure) {
        let mut state = self.lock();
        state.last_failure = Some(failure);
        state.phase = RtmpRelayPhase::Backoff;
    }

    fn wait_backoff(&self) -> bool {
        let state = self.lock();
        let (state, _) = self
            .available
            .wait_timeout_while(state, self.config.reconnect_interval, |state| {
                state.accepting
            })
            .expect("RTMP relay state mutex poisoned during backoff");
        state.accepting
    }

    fn next_event(&self) -> Option<MediaEvent> {
        let state = self.lock();
        let mut state = self
            .available
            .wait_timeout_while(state, RELAY_QUEUE_POLL, |state| {
                state.accepting && state.queue.is_empty()
            })
            .expect("RTMP relay state mutex poisoned while waiting")
            .0;
        let event = state.queue.pop_front()?;
        state.queue_bytes -= event.payload_len();
        Some(event)
    }

    fn take_bootstrap(&self) -> Vec<MediaEvent> {
        let mut state = self.lock();
        let discarded = u64::try_from(state.queue.len()).unwrap_or(u64::MAX);
        state.events_dropped = state.events_dropped.saturating_add(discarded);
        state.queue.clear();
        state.queue_bytes = 0;
        state.cache.bootstrap()
    }

    fn record_sent(&self, event: &MediaEvent) {
        let mut state = self.lock();
        state.events_sent = state.events_sent.saturating_add(1);
        state.payload_bytes_sent = state
            .payload_bytes_sent
            .saturating_add(event.payload_len() as u64);
    }
}

impl RelayCache {
    fn update(&mut self, event: &MediaEvent) -> bool {
        let mut codec_header_changed = false;
        match event.kind() {
            MediaEventKind::Metadata => self.metadata = Some(event.clone()),
            MediaEventKind::AacSequenceHeader => self.aac_header = Some(event.clone()),
            MediaEventKind::AvcSequenceHeader
            | MediaEventKind::HevcSequenceHeader
            | MediaEventKind::Av1SequenceHeader => {
                if let Some(codec) = event.video_codec() {
                    self.keyframe = None;
                    self.video_headers.insert(codec, event.clone());
                    codec_header_changed = true;
                }
            }
            MediaEventKind::VideoKeyframe => self.keyframe = Some(event.clone()),
            MediaEventKind::Audio => self.latest_audio = Some(event.clone()),
            MediaEventKind::VideoInterframe | MediaEventKind::VideoDisposable => {}
        }
        codec_header_changed
    }

    fn bootstrap(&self) -> Vec<MediaEvent> {
        let mut events = Vec::with_capacity(4);
        events.extend(self.metadata.iter().cloned());
        events.extend(self.aac_header.iter().cloned());
        if let Some(keyframe) = &self.keyframe {
            events.extend(
                keyframe
                    .video_codec()
                    .and_then(|codec| self.video_headers.get(&codec))
                    .cloned(),
            );
            events.push(keyframe.clone());
        } else {
            events.extend(self.latest_audio.iter().cloned());
        }
        events
    }
}

struct RelayExecutor {
    sender: SyncSender<Arc<RelayShared>>,
}

impl RelayExecutor {
    fn new() -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Arc<RelayShared>>(MAX_QUEUED_RTMP_RELAYS);
        let receiver = Arc::new(Mutex::new(receiver));
        for index in 0..RTMP_RELAY_WORKER_THREADS {
            let receiver = Arc::clone(&receiver);
            thread::Builder::new()
                .name(format!("rtmp-relay-worker-{index}"))
                .spawn(move || {
                    loop {
                        let task = receiver
                            .lock()
                            .expect("RTMP relay executor mutex poisoned")
                            .recv();
                        let Ok(shared) = task else {
                            return;
                        };
                        run_relay(&shared);
                    }
                })
                .expect("shared RTMP relay worker must start");
        }
        Self { sender }
    }

    fn admit(&self, shared: Arc<RelayShared>) -> Result<(), ()> {
        self.sender.try_send(shared).map_err(|error| match error {
            TrySendError::Full(_) | TrySendError::Disconnected(_) => (),
        })
    }
}

fn relay_executor() -> &'static RelayExecutor {
    static EXECUTOR: OnceLock<RelayExecutor> = OnceLock::new();
    EXECUTOR.get_or_init(RelayExecutor::new)
}

fn fits_queue(events: &[MediaEvent], config: RtmpRelayConfig) -> bool {
    events.len() <= config.max_queue_messages
        && events
            .iter()
            .try_fold(0_usize, |total, event| {
                total.checked_add(event.payload_len())
            })
            .is_some_and(|bytes| bytes <= config.max_queue_bytes)
}

fn run_relay(shared: &RelayShared) {
    while shared.is_accepting() {
        {
            let mut state = shared.lock();
            state.phase = RtmpRelayPhase::Connecting;
            state.connection_attempts = state.connection_attempts.saturating_add(1);
        }
        let Ok(mut stream) =
            TcpStream::connect_timeout(&shared.destination.address, shared.config.connect_timeout)
        else {
            shared.record_failure(RtmpRelayFailure::Connect);
            if !shared.wait_backoff() {
                break;
            }
            continue;
        };
        if establish_transport(&mut stream, shared.config.handshake_timeout).is_err() {
            shared.record_failure(RtmpRelayFailure::Handshake);
            if !shared.wait_backoff() {
                break;
            }
            continue;
        }
        let mut session = match establish_publish_session(&mut stream, &shared.destination) {
            Ok(session) => session,
            Err(failure) => {
                shared.record_failure(failure);
                if !shared.wait_backoff() {
                    break;
                }
                continue;
            }
        };
        {
            let mut state = shared.lock();
            state.connections = state.connections.saturating_add(1);
            state.reconnects = state.connections.saturating_sub(1);
            state.phase = RtmpRelayPhase::Publishing;
            state.last_failure = None;
        }

        let mut failed = None;
        for event in shared.take_bootstrap() {
            if let Err(failure) = publish_event(&mut stream, &mut session, &event) {
                failed = Some(failure);
                break;
            }
            shared.record_sent(&event);
        }
        while failed.is_none() && shared.is_accepting() {
            if let Err(failure) = process_peer_input(&mut stream, &mut session) {
                failed = Some(failure);
                break;
            }
            let Some(event) = shared.next_event() else {
                continue;
            };
            if let Err(failure) = publish_event(&mut stream, &mut session, &event) {
                failed = Some(failure);
                break;
            }
            shared.record_sent(&event);
        }
        if let Some(failure) = failed {
            shared.record_failure(failure);
            if !shared.wait_backoff() {
                break;
            }
        }
    }
    shared.set_phase(RtmpRelayPhase::Stopped);
}

fn establish_transport(stream: &mut TcpStream, timeout: Duration) -> io::Result<()> {
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.set_nodelay(true)?;
    let mut handshake = Handshake::new(PeerType::Client);
    let hello = handshake
        .generate_outbound_p0_and_p1()
        .map_err(io::Error::other)?;
    stream.write_all(&hello)?;
    let mut response = [0; HANDSHAKE_RESPONSE_BYTES];
    stream.read_exact(&mut response)?;
    let finish = match handshake
        .process_bytes(&response)
        .map_err(io::Error::other)?
    {
        HandshakeProcessResult::Completed { response_bytes, .. } => response_bytes,
        HandshakeProcessResult::InProgress { .. } => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "RTMP server handshake remained incomplete",
            ));
        }
    };
    stream.write_all(&finish)?;
    stream.flush()
}

fn establish_publish_session(
    stream: &mut TcpStream,
    destination: &RtmpDestination,
) -> Result<ClientSession, RtmpRelayFailure> {
    let mut config = ClientSessionConfig::new();
    config.chunk_size = 4_096;
    config.tc_url = Some(format!(
        "rtmp://{}/{}",
        destination.address, destination.application
    ));
    let (mut session, initial) =
        ClientSession::new(config).map_err(|_| RtmpRelayFailure::Session)?;
    write_results(stream, initial)?;
    let connect = session
        .request_connection(destination.application.clone())
        .map_err(|_| RtmpRelayFailure::Session)?;
    await_event(stream, &mut session, vec![connect], |event| {
        matches!(event, ClientSessionEvent::ConnectionRequestAccepted)
    })?;
    let publish = session
        .request_publishing(destination.stream_name.clone(), PublishRequestType::Live)
        .map_err(|_| RtmpRelayFailure::Session)?;
    await_event(stream, &mut session, vec![publish], |event| {
        matches!(event, ClientSessionEvent::PublishRequestAccepted)
    })?;
    stream
        .set_read_timeout(Some(Duration::from_millis(1)))
        .map_err(|_| RtmpRelayFailure::Transport)?;
    Ok(session)
}

fn await_event(
    stream: &mut TcpStream,
    session: &mut ClientSession,
    initial: Vec<ClientSessionResult>,
    predicate: impl Fn(&ClientSessionEvent) -> bool,
) -> Result<(), RtmpRelayFailure> {
    write_results(stream, initial)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut buffer = [0; RELAY_READ_BUFFER_BYTES];
    while Instant::now() < deadline {
        let count = stream
            .read(&mut buffer)
            .map_err(|_| RtmpRelayFailure::Transport)?;
        if count == 0 {
            return Err(RtmpRelayFailure::Transport);
        }
        let results = session
            .handle_input(&buffer[..count])
            .map_err(|_| RtmpRelayFailure::Session)?;
        let mut accepted = false;
        let mut outbound = Vec::new();
        for result in results {
            match result {
                ClientSessionResult::OutboundResponse(packet) => outbound.push(packet.bytes),
                ClientSessionResult::RaisedEvent(event) => accepted |= predicate(&event),
                ClientSessionResult::UnhandleableMessageReceived(_) => {}
            }
        }
        write_packets(stream, outbound)?;
        if accepted {
            return Ok(());
        }
    }
    Err(RtmpRelayFailure::Session)
}

fn process_peer_input(
    stream: &mut TcpStream,
    session: &mut ClientSession,
) -> Result<(), RtmpRelayFailure> {
    let mut buffer = [0; RELAY_READ_BUFFER_BYTES];
    match stream.read(&mut buffer) {
        Ok(0) => Err(RtmpRelayFailure::Transport),
        Ok(count) => {
            let results = session
                .handle_input(&buffer[..count])
                .map_err(|_| RtmpRelayFailure::Session)?;
            write_results(stream, results)
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            Ok(())
        }
        Err(_) => Err(RtmpRelayFailure::Transport),
    }
}

fn publish_event(
    stream: &mut TcpStream,
    session: &mut ClientSession,
    event: &MediaEvent,
) -> Result<(), RtmpRelayFailure> {
    let result = match event.kind() {
        MediaEventKind::Metadata => session.publish_metadata(
            event
                .stream_metadata()
                .expect("metadata events retain decoded metadata"),
        ),
        MediaEventKind::AacSequenceHeader | MediaEventKind::Audio => session.publish_audio_data(
            Bytes::copy_from_slice(event.payload()),
            RtmpTimestamp::new(event.timestamp_ms()),
            false,
        ),
        MediaEventKind::AvcSequenceHeader
        | MediaEventKind::HevcSequenceHeader
        | MediaEventKind::Av1SequenceHeader
        | MediaEventKind::VideoKeyframe
        | MediaEventKind::VideoInterframe
        | MediaEventKind::VideoDisposable => session.publish_video_data(
            Bytes::copy_from_slice(event.payload()),
            RtmpTimestamp::new(event.timestamp_ms()),
            event.kind() == MediaEventKind::VideoDisposable,
        ),
    }
    .map_err(|_| RtmpRelayFailure::Session)?;
    write_results(stream, vec![result])
}

fn write_results(
    stream: &mut TcpStream,
    results: Vec<ClientSessionResult>,
) -> Result<(), RtmpRelayFailure> {
    write_packets(
        stream,
        results.into_iter().filter_map(|result| match result {
            ClientSessionResult::OutboundResponse(packet) => Some(packet.bytes),
            ClientSessionResult::RaisedEvent(_)
            | ClientSessionResult::UnhandleableMessageReceived(_) => None,
        }),
    )
}

fn write_packets(
    stream: &mut TcpStream,
    packets: impl IntoIterator<Item = Vec<u8>>,
) -> Result<(), RtmpRelayFailure> {
    for packet in packets {
        stream
            .write_all(&packet)
            .map_err(|_| RtmpRelayFailure::Transport)?;
    }
    stream.flush().map_err(|_| RtmpRelayFailure::Transport)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overflow_and_codec_headers_gate_relay_output_until_a_matching_keyframe() {
        let controller = RtmpRelayController {
            shared: Arc::new(RelayShared {
                destination: RtmpDestination {
                    address: "127.0.0.1:1935".parse().expect("destination"),
                    application: "live".into(),
                    stream_name: "camera".into(),
                },
                config: RtmpRelayConfig {
                    max_queue_messages: 4,
                    max_queue_bytes: 128,
                    ..RtmpRelayConfig::default()
                },
                state: Mutex::new(RelayState {
                    accepting: true,
                    waiting_for_keyframe: false,
                    queue: VecDeque::new(),
                    queue_bytes: 0,
                    cache: RelayCache::default(),
                    phase: RtmpRelayPhase::Publishing,
                    last_failure: None,
                    connection_attempts: 0,
                    connections: 0,
                    reconnects: 0,
                    events_enqueued: 0,
                    events_sent: 0,
                    events_dropped: 0,
                    payload_bytes_sent: 0,
                }),
                available: Condvar::new(),
            }),
        };

        controller.try_enqueue(video(0, &[0x17, 0x00, 0, 0, 0, 0x01]));
        controller.try_enqueue(video(1, &[0x17, 0x01, 0, 0, 0, 0x11]));
        for timestamp in 2..=5 {
            controller.try_enqueue(video(timestamp, &[0x27, 0x01, 0, 0, 0, 0x22]));
        }
        controller.try_enqueue(
            MediaEvent::audio(6, Arc::<[u8]>::from([0xaf, 0x01, 0x33])).expect("audio"),
        );
        controller.try_enqueue(video(7, &[0x27, 0x01, 0, 0, 0, 0x44]));
        {
            let state = controller.shared.lock();
            assert!(state.waiting_for_keyframe);
            assert!(state.queue.is_empty());
        }

        controller.try_enqueue(video(8, &[0x17, 0x00, 0, 0, 0, 0x02]));
        controller.try_enqueue(video(9, &[0x17, 0x01, 0, 0, 0, 0x55]));

        let state = controller.shared.lock();
        assert!(!state.waiting_for_keyframe);
        assert_eq!(state.queue.len(), 2);
        assert_eq!(state.queue[0].payload(), [0x17, 0x00, 0, 0, 0, 0x02]);
        assert_eq!(state.queue[1].payload(), [0x17, 0x01, 0, 0, 0, 0x55]);
    }

    fn video(timestamp: u32, payload: &[u8]) -> MediaEvent {
        MediaEvent::video(timestamp, Arc::<[u8]>::from(payload)).expect("video event")
    }
}
