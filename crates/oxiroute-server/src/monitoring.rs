use std::{
    collections::{HashMap, hash_map::Entry},
    error::Error,
    fmt, io,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Instant, SystemTime},
};

#[cfg(target_os = "linux")]
use std::fs;

use serde::Serialize;

#[derive(Debug)]
pub enum MetricsError {
    DuplicateListener(String),
    ListenerNotFound(String),
    InvalidListenerField(&'static str),
    ConnectionLimitReached {
        listener: String,
        limit: u64,
    },
    CounterOverflow(&'static str),
    StatePoisoned(&'static str),
    UnsupportedPlatform(&'static str),
    Io {
        path: &'static str,
        source: io::Error,
    },
    InvalidData {
        resource: &'static str,
        detail: String,
    },
    SystemClockBeforeUnixEpoch,
    ValueOutOfRange(&'static str),
}

impl fmt::Display for MetricsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateListener(name) => {
                write!(formatter, "listener `{name}` is already registered")
            }
            Self::ListenerNotFound(name) => {
                write!(formatter, "listener `{name}` is not registered")
            }
            Self::InvalidListenerField(field) => {
                write!(formatter, "listener {field} must not be empty")
            }
            Self::ConnectionLimitReached { listener, limit } => {
                write!(
                    formatter,
                    "listener `{listener}` reached its {limit}-connection limit"
                )
            }
            Self::CounterOverflow(counter) => {
                write!(formatter, "metrics counter `{counter}` overflowed")
            }
            Self::StatePoisoned(state) => write!(formatter, "metrics state `{state}` is poisoned"),
            Self::UnsupportedPlatform(platform) => {
                write!(
                    formatter,
                    "process and host sampling is unsupported on {platform}"
                )
            }
            Self::Io { path, source } => write!(formatter, "failed to read `{path}`: {source}"),
            Self::InvalidData { resource, detail } => {
                write!(formatter, "invalid data in `{resource}`: {detail}")
            }
            Self::SystemClockBeforeUnixEpoch => {
                formatter.write_str("system clock predates the Unix epoch")
            }
            Self::ValueOutOfRange(value) => {
                write!(
                    formatter,
                    "sampled value `{value}` exceeds the supported range"
                )
            }
        }
    }
}

impl Error for MetricsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct RuntimeMetrics {
    inner: Arc<RuntimeMetricsInner>,
}

struct RuntimeMetricsInner {
    started_at: Instant,
    listeners: RwLock<HashMap<String, Arc<ListenerState>>>,
    previous_cpu_sample: Mutex<Option<CpuSample>>,
}

impl RuntimeMetrics {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RuntimeMetricsInner {
                started_at: Instant::now(),
                listeners: RwLock::new(HashMap::new()),
                previous_cpu_sample: Mutex::new(None),
            }),
        }
    }

    /// Registers a listener and returns its accounting handle.
    ///
    /// # Errors
    ///
    /// Returns an error when any field is empty, the listener name is already registered, or the
    /// listener registry is poisoned.
    pub fn register_listener(
        &self,
        name: impl Into<String>,
        protocol: impl Into<String>,
        bind: impl Into<String>,
        max_connections: u64,
    ) -> Result<ListenerMetrics, MetricsError> {
        let name = name.into();
        let protocol = protocol.into();
        let bind = bind.into();
        validate_listener_field("name", &name)?;
        validate_listener_field("protocol", &protocol)?;
        validate_listener_field("bind", &bind)?;
        if max_connections == 0 {
            return Err(MetricsError::InvalidListenerField("max_connections"));
        }

        let mut listeners = self
            .inner
            .listeners
            .write()
            .map_err(|_| MetricsError::StatePoisoned("listeners"))?;
        match listeners.entry(name.clone()) {
            Entry::Vacant(entry) => {
                let state = Arc::new(ListenerState::new(name, protocol, bind, max_connections));
                entry.insert(Arc::clone(&state));
                Ok(ListenerMetrics { state })
            }
            Entry::Occupied(_) => Err(MetricsError::DuplicateListener(name)),
        }
    }

    /// Returns the accounting handle for a registered listener.
    ///
    /// # Errors
    ///
    /// Returns an error when the listener registry is poisoned.
    pub fn listener(&self, name: &str) -> Result<Option<ListenerMetrics>, MetricsError> {
        let listeners = self
            .inner
            .listeners
            .read()
            .map_err(|_| MetricsError::StatePoisoned("listeners"))?;
        Ok(listeners.get(name).map(|state| ListenerMetrics {
            state: Arc::clone(state),
        }))
    }

    /// Accounts for a newly accepted connection on a named listener.
    ///
    /// The returned guard decrements the active connection count when dropped.
    ///
    /// # Errors
    ///
    /// Returns an error when the listener is unknown, the registry is poisoned, or a counter would
    /// overflow.
    pub fn begin_connection(&self, listener_name: &str) -> Result<ConnectionGuard, MetricsError> {
        self.listener(listener_name)?
            .ok_or_else(|| MetricsError::ListenerNotFound(listener_name.to_owned()))?
            .begin_connection()
    }

    /// Samples process, host, traffic, and listener metrics.
    ///
    /// CPU percent is `None` on the first successful sample and whenever no aggregate host CPU tick
    /// elapsed since the previous sample.
    ///
    /// # Errors
    ///
    /// Returns an error when metrics state cannot be read, the system clock is invalid, aggregate
    /// counters overflow, the current platform is unsupported, or an operating-system metric cannot
    /// be read or parsed truthfully.
    pub fn snapshot(&self) -> Result<RuntimeSnapshot, MetricsError> {
        let (traffic, listeners) = self.counter_snapshots()?;
        let mut previous_cpu_sample = self
            .inner
            .previous_cpu_sample
            .lock()
            .map_err(|_| MetricsError::StatePoisoned("previous CPU sample"))?;
        let system = sample_system()?;
        let cpu_percent = cpu_percent(previous_cpu_sample.as_ref(), &system.cpu)?;
        let sampled_at_unix_ms = unix_time_ms()?;
        let uptime_ms = u64::try_from(self.inner.started_at.elapsed().as_millis())
            .map_err(|_| MetricsError::ValueOutOfRange("uptime milliseconds"))?;

        let SystemSample {
            resident_memory_bytes,
            virtual_memory_bytes,
            thread_count,
            open_file_descriptors,
            host,
            cpu,
        } = system;
        let snapshot = RuntimeSnapshot {
            sampled_at_unix_ms,
            uptime_ms,
            process: ProcessSnapshot {
                cpu_percent,
                resident_memory_bytes,
                virtual_memory_bytes,
                thread_count,
                open_file_descriptors,
            },
            host,
            traffic,
            listeners,
        };
        *previous_cpu_sample = Some(cpu);
        Ok(snapshot)
    }

    fn counter_snapshots(&self) -> Result<(TrafficSnapshot, Vec<ListenerSnapshot>), MetricsError> {
        let listeners = self
            .inner
            .listeners
            .read()
            .map_err(|_| MetricsError::StatePoisoned("listeners"))?;
        let mut states: Vec<_> = listeners.values().cloned().collect();
        drop(listeners);
        states.sort_unstable_by(|left, right| left.name.cmp(&right.name));

        let mut traffic = TrafficSnapshot::default();
        let mut snapshots = Vec::with_capacity(states.len());
        for state in states {
            let snapshot = state.snapshot();
            add_total(
                &mut traffic.accepted_connections,
                snapshot.accepted_connections,
                "traffic.acceptedConnections",
            )?;
            add_total(
                &mut traffic.active_connections,
                snapshot.active_connections,
                "traffic.activeConnections",
            )?;
            add_total(
                &mut traffic.bytes_received,
                snapshot.bytes_received,
                "traffic.bytesReceived",
            )?;
            add_total(
                &mut traffic.bytes_sent,
                snapshot.bytes_sent,
                "traffic.bytesSent",
            )?;
            snapshots.push(snapshot);
        }
        Ok((traffic, snapshots))
    }
}

impl Default for RuntimeMetrics {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct ListenerMetrics {
    state: Arc<ListenerState>,
}

impl ListenerMetrics {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.state.name
    }

    #[must_use]
    pub fn protocol(&self) -> &str {
        &self.state.protocol
    }

    #[must_use]
    pub fn bind(&self) -> &str {
        &self.state.bind
    }

    /// Accounts for a newly accepted connection.
    ///
    /// The returned guard decrements the active connection count when dropped, including during
    /// unwinding.
    ///
    /// # Errors
    ///
    /// Returns an error if the accepted or active connection counter would overflow.
    pub fn begin_connection(&self) -> Result<ConnectionGuard, MetricsError> {
        checked_atomic_add(
            &self.state.accepted_connections,
            1,
            "listener.acceptedConnections",
        )?;
        self.state
            .active_connections
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                (current < self.state.max_connections)
                    .then(|| current.checked_add(1))
                    .flatten()
            })
            .map_err(|current| {
                if current >= self.state.max_connections {
                    MetricsError::ConnectionLimitReached {
                        listener: self.state.name.clone(),
                        limit: self.state.max_connections,
                    }
                } else {
                    MetricsError::CounterOverflow("listener.activeConnections")
                }
            })?;
        Ok(ConnectionGuard {
            state: Arc::clone(&self.state),
        })
    }

    /// Adds bytes read across this listener to its traffic total.
    ///
    /// # Errors
    ///
    /// Returns an error if the byte counter would overflow.
    pub fn record_bytes_received(&self, bytes: u64) -> Result<(), MetricsError> {
        checked_atomic_add(&self.state.bytes_received, bytes, "listener.bytesReceived")
    }

    /// Adds bytes written across this listener to its traffic total.
    ///
    /// # Errors
    ///
    /// Returns an error if the byte counter would overflow.
    pub fn record_bytes_sent(&self, bytes: u64) -> Result<(), MetricsError> {
        checked_atomic_add(&self.state.bytes_sent, bytes, "listener.bytesSent")
    }
}

pub struct ConnectionGuard {
    state: Arc<ListenerState>,
}

impl ConnectionGuard {
    #[must_use]
    pub fn listener_name(&self) -> &str {
        &self.state.name
    }

    /// Adds bytes read from this connection to its listener total.
    ///
    /// # Errors
    ///
    /// Returns an error if the byte counter would overflow.
    pub fn record_bytes_received(&self, bytes: u64) -> Result<(), MetricsError> {
        checked_atomic_add(&self.state.bytes_received, bytes, "listener.bytesReceived")
    }

    /// Adds bytes written to this connection to its listener total.
    ///
    /// # Errors
    ///
    /// Returns an error if the byte counter would overflow.
    pub fn record_bytes_sent(&self, bytes: u64) -> Result<(), MetricsError> {
        checked_atomic_add(&self.state.bytes_sent, bytes, "listener.bytesSent")
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        decrement_counter(&self.state.active_connections);
    }
}

struct ListenerState {
    name: String,
    protocol: String,
    bind: String,
    max_connections: u64,
    accepted_connections: AtomicU64,
    active_connections: AtomicU64,
    bytes_received: AtomicU64,
    bytes_sent: AtomicU64,
}

impl ListenerState {
    fn new(name: String, protocol: String, bind: String, max_connections: u64) -> Self {
        Self {
            name,
            protocol,
            bind,
            max_connections,
            accepted_connections: AtomicU64::new(0),
            active_connections: AtomicU64::new(0),
            bytes_received: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
        }
    }

    fn snapshot(&self) -> ListenerSnapshot {
        ListenerSnapshot {
            name: self.name.clone(),
            protocol: self.protocol.clone(),
            bind: self.bind.clone(),
            max_connections: self.max_connections,
            accepted_connections: self.accepted_connections.load(Ordering::Relaxed),
            active_connections: self.active_connections.load(Ordering::Relaxed),
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSnapshot {
    pub sampled_at_unix_ms: u64,
    pub uptime_ms: u64,
    pub process: ProcessSnapshot,
    pub host: HostSnapshot,
    pub traffic: TrafficSnapshot,
    pub listeners: Vec<ListenerSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessSnapshot {
    pub cpu_percent: Option<f64>,
    pub resident_memory_bytes: u64,
    pub virtual_memory_bytes: u64,
    pub thread_count: u64,
    pub open_file_descriptors: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostSnapshot {
    pub load_average_1m: f64,
    pub load_average_5m: f64,
    pub load_average_15m: f64,
    pub total_memory_bytes: u64,
    pub available_memory_bytes: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrafficSnapshot {
    pub accepted_connections: u64,
    pub active_connections: u64,
    pub bytes_received: u64,
    pub bytes_sent: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListenerSnapshot {
    pub name: String,
    pub protocol: String,
    pub bind: String,
    pub max_connections: u64,
    pub accepted_connections: u64,
    pub active_connections: u64,
    pub bytes_received: u64,
    pub bytes_sent: u64,
}

fn validate_listener_field(field: &'static str, value: &str) -> Result<(), MetricsError> {
    if value.trim().is_empty() {
        return Err(MetricsError::InvalidListenerField(field));
    }
    Ok(())
}

fn checked_atomic_add(
    counter: &AtomicU64,
    value: u64,
    name: &'static str,
) -> Result<(), MetricsError> {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(value)
        })
        .map(|_| ())
        .map_err(|_| MetricsError::CounterOverflow(name))
}

fn decrement_counter(counter: &AtomicU64) {
    let result = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        current.checked_sub(1)
    });
    debug_assert!(result.is_ok(), "active connection counter underflowed");
}

fn add_total(total: &mut u64, value: u64, name: &'static str) -> Result<(), MetricsError> {
    *total = total
        .checked_add(value)
        .ok_or(MetricsError::CounterOverflow(name))?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CpuSample {
    process_ticks: u64,
    system_ticks: u64,
    logical_cpu_count: u32,
}

struct SystemSample {
    resident_memory_bytes: u64,
    virtual_memory_bytes: u64,
    thread_count: u64,
    open_file_descriptors: u64,
    host: HostSnapshot,
    cpu: CpuSample,
}

#[cfg(target_os = "linux")]
fn sample_system() -> Result<SystemSample, MetricsError> {
    const PROCESS_STAT: &str = "/proc/self/stat";
    const PROCESS_STATUS: &str = "/proc/self/status";
    const SYSTEM_STAT: &str = "/proc/stat";
    const LOAD_AVERAGE: &str = "/proc/loadavg";
    const MEMORY_INFO: &str = "/proc/meminfo";

    let process_ticks = parse_process_stat(&read_proc(PROCESS_STAT)?)?;
    let mut cpu = parse_system_stat(&read_proc(SYSTEM_STAT)?)?;
    cpu.process_ticks = process_ticks;
    let status = parse_process_status(&read_proc(PROCESS_STATUS)?)?;
    let load_average = parse_load_average(&read_proc(LOAD_AVERAGE)?)?;
    let memory = parse_memory_info(&read_proc(MEMORY_INFO)?)?;

    Ok(SystemSample {
        resident_memory_bytes: status.resident_memory_bytes,
        virtual_memory_bytes: status.virtual_memory_bytes,
        thread_count: status.thread_count,
        open_file_descriptors: count_open_file_descriptors()?,
        host: HostSnapshot {
            load_average_1m: load_average[0],
            load_average_5m: load_average[1],
            load_average_15m: load_average[2],
            total_memory_bytes: memory.total_memory_bytes,
            available_memory_bytes: memory.available_memory_bytes,
        },
        cpu,
    })
}

#[cfg(not(target_os = "linux"))]
fn sample_system() -> Result<SystemSample, MetricsError> {
    Err(MetricsError::UnsupportedPlatform(std::env::consts::OS))
}

#[cfg(target_os = "linux")]
fn read_proc(path: &'static str) -> Result<String, MetricsError> {
    fs::read_to_string(path).map_err(|source| MetricsError::Io { path, source })
}

#[cfg(target_os = "linux")]
fn count_open_file_descriptors() -> Result<u64, MetricsError> {
    const FILE_DESCRIPTORS: &str = "/proc/self/fd";

    let entries = fs::read_dir(FILE_DESCRIPTORS).map_err(|source| MetricsError::Io {
        path: FILE_DESCRIPTORS,
        source,
    })?;
    let mut count = 0_u64;
    for entry in entries {
        entry.map_err(|source| MetricsError::Io {
            path: FILE_DESCRIPTORS,
            source,
        })?;
        count = count
            .checked_add(1)
            .ok_or(MetricsError::CounterOverflow("process.openFileDescriptors"))?;
    }

    // read_dir itself owns one descriptor that is visible in /proc/self/fd during enumeration.
    count.checked_sub(1).ok_or_else(|| {
        invalid_data(
            FILE_DESCRIPTORS,
            "enumeration did not include its own directory descriptor",
        )
    })
}

#[cfg(target_os = "linux")]
#[derive(Debug, Eq, PartialEq)]
struct ProcessStatus {
    resident_memory_bytes: u64,
    virtual_memory_bytes: u64,
    thread_count: u64,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Eq, PartialEq)]
struct MemoryInfo {
    total_memory_bytes: u64,
    available_memory_bytes: u64,
}

#[cfg(target_os = "linux")]
fn parse_process_stat(input: &str) -> Result<u64, MetricsError> {
    const RESOURCE: &str = "/proc/self/stat";

    let command_start = input
        .find('(')
        .ok_or_else(|| invalid_data(RESOURCE, "missing process command"))?;
    let command_end = input
        .rfind(')')
        .filter(|end| *end > command_start)
        .ok_or_else(|| invalid_data(RESOURCE, "unterminated process command"))?;
    parse_u64(input[..command_start].trim(), RESOURCE, "pid")?;

    let fields: Vec<_> = input[command_end + 1..].split_whitespace().collect();
    if fields.len() < 13 {
        return Err(invalid_data(
            RESOURCE,
            "expected fields through process system CPU time",
        ));
    }
    if fields[0].chars().count() != 1 {
        return Err(invalid_data(RESOURCE, "invalid process state field"));
    }
    let user_ticks = parse_u64(fields[11], RESOURCE, "utime")?;
    let system_ticks = parse_u64(fields[12], RESOURCE, "stime")?;
    user_ticks
        .checked_add(system_ticks)
        .ok_or_else(|| invalid_data(RESOURCE, "process CPU tick total overflowed"))
}

#[cfg(target_os = "linux")]
fn parse_system_stat(input: &str) -> Result<CpuSample, MetricsError> {
    const RESOURCE: &str = "/proc/stat";

    let mut system_ticks = None;
    let mut logical_cpu_count = 0_u32;
    for line in input.lines() {
        let mut fields = line.split_whitespace();
        let Some(name) = fields.next() else {
            continue;
        };
        if name == "cpu" {
            if system_ticks.is_some() {
                return Err(invalid_data(RESOURCE, "duplicate aggregate CPU row"));
            }
            let mut total = 0_u64;
            for _ in 0..8 {
                let value = fields.next().ok_or_else(|| {
                    invalid_data(RESOURCE, "aggregate CPU row has fewer than eight counters")
                })?;
                total = total
                    .checked_add(parse_u64(value, RESOURCE, "aggregate CPU counter")?)
                    .ok_or_else(|| invalid_data(RESOURCE, "aggregate CPU tick total overflowed"))?;
            }
            system_ticks = Some(total);
        } else if name.strip_prefix("cpu").is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        }) {
            logical_cpu_count = logical_cpu_count
                .checked_add(1)
                .ok_or_else(|| invalid_data(RESOURCE, "logical CPU count overflowed"))?;
        }
    }

    let system_ticks =
        system_ticks.ok_or_else(|| invalid_data(RESOURCE, "missing aggregate CPU row"))?;
    if logical_cpu_count == 0 {
        return Err(invalid_data(RESOURCE, "missing logical CPU rows"));
    }
    Ok(CpuSample {
        process_ticks: 0,
        system_ticks,
        logical_cpu_count,
    })
}

#[cfg(target_os = "linux")]
fn parse_process_status(input: &str) -> Result<ProcessStatus, MetricsError> {
    const RESOURCE: &str = "/proc/self/status";

    let mut resident_memory_bytes = None;
    let mut virtual_memory_bytes = None;
    let mut thread_count = None;
    for line in input.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        match key {
            "VmRSS" => set_once(
                &mut resident_memory_bytes,
                parse_kib(value, RESOURCE, "VmRSS")?,
                RESOURCE,
                "VmRSS",
            )?,
            "VmSize" => set_once(
                &mut virtual_memory_bytes,
                parse_kib(value, RESOURCE, "VmSize")?,
                RESOURCE,
                "VmSize",
            )?,
            "Threads" => set_once(
                &mut thread_count,
                parse_single_u64(value, RESOURCE, "Threads")?,
                RESOURCE,
                "Threads",
            )?,
            _ => {}
        }
    }

    Ok(ProcessStatus {
        resident_memory_bytes: required(resident_memory_bytes, RESOURCE, "VmRSS")?,
        virtual_memory_bytes: required(virtual_memory_bytes, RESOURCE, "VmSize")?,
        thread_count: required(thread_count, RESOURCE, "Threads")?,
    })
}

#[cfg(target_os = "linux")]
fn parse_load_average(input: &str) -> Result<[f64; 3], MetricsError> {
    const RESOURCE: &str = "/proc/loadavg";

    let mut fields = input.split_whitespace();
    Ok([
        parse_load_value(fields.next(), RESOURCE, "one-minute load average")?,
        parse_load_value(fields.next(), RESOURCE, "five-minute load average")?,
        parse_load_value(fields.next(), RESOURCE, "fifteen-minute load average")?,
    ])
}

#[cfg(target_os = "linux")]
fn parse_memory_info(input: &str) -> Result<MemoryInfo, MetricsError> {
    const RESOURCE: &str = "/proc/meminfo";

    let mut total_memory_bytes = None;
    let mut available_memory_bytes = None;
    for line in input.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        match key {
            "MemTotal" => set_once(
                &mut total_memory_bytes,
                parse_kib(value, RESOURCE, "MemTotal")?,
                RESOURCE,
                "MemTotal",
            )?,
            "MemAvailable" => set_once(
                &mut available_memory_bytes,
                parse_kib(value, RESOURCE, "MemAvailable")?,
                RESOURCE,
                "MemAvailable",
            )?,
            _ => {}
        }
    }

    Ok(MemoryInfo {
        total_memory_bytes: required(total_memory_bytes, RESOURCE, "MemTotal")?,
        available_memory_bytes: required(available_memory_bytes, RESOURCE, "MemAvailable")?,
    })
}

#[cfg(target_os = "linux")]
fn set_once<T>(
    destination: &mut Option<T>,
    value: T,
    resource: &'static str,
    field: &'static str,
) -> Result<(), MetricsError> {
    if destination.replace(value).is_some() {
        return Err(invalid_data(resource, format!("duplicate `{field}` field")));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn required<T>(
    value: Option<T>,
    resource: &'static str,
    field: &'static str,
) -> Result<T, MetricsError> {
    value.ok_or_else(|| invalid_data(resource, format!("missing `{field}` field")))
}

#[cfg(target_os = "linux")]
fn parse_kib(
    value: &str,
    resource: &'static str,
    field: &'static str,
) -> Result<u64, MetricsError> {
    let mut fields = value.split_whitespace();
    let kib = fields
        .next()
        .ok_or_else(|| invalid_data(resource, format!("missing `{field}` value")))?;
    if fields.next() != Some("kB") || fields.next().is_some() {
        return Err(invalid_data(
            resource,
            format!("`{field}` must contain one value in kB"),
        ));
    }
    parse_u64(kib, resource, field)?
        .checked_mul(1024)
        .ok_or_else(|| invalid_data(resource, format!("`{field}` byte value overflowed")))
}

#[cfg(target_os = "linux")]
fn parse_single_u64(
    value: &str,
    resource: &'static str,
    field: &'static str,
) -> Result<u64, MetricsError> {
    let mut fields = value.split_whitespace();
    let value = fields
        .next()
        .ok_or_else(|| invalid_data(resource, format!("missing `{field}` value")))?;
    if fields.next().is_some() {
        return Err(invalid_data(
            resource,
            format!("`{field}` must contain one integer"),
        ));
    }
    parse_u64(value, resource, field)
}

#[cfg(target_os = "linux")]
fn parse_u64(
    value: &str,
    resource: &'static str,
    field: &'static str,
) -> Result<u64, MetricsError> {
    value.parse().map_err(|_| {
        invalid_data(
            resource,
            format!("`{field}` contains invalid integer `{value}`"),
        )
    })
}

#[cfg(target_os = "linux")]
fn parse_load_value(
    value: Option<&str>,
    resource: &'static str,
    field: &'static str,
) -> Result<f64, MetricsError> {
    let value = value.ok_or_else(|| invalid_data(resource, format!("missing {field}")))?;
    let parsed: f64 = value.parse().map_err(|_| {
        invalid_data(
            resource,
            format!("{field} contains invalid number `{value}`"),
        )
    })?;
    if !parsed.is_finite() || parsed.is_sign_negative() {
        return Err(invalid_data(
            resource,
            format!("{field} must be a finite non-negative number"),
        ));
    }
    Ok(parsed)
}

fn invalid_data(resource: &'static str, detail: impl Into<String>) -> MetricsError {
    MetricsError::InvalidData {
        resource,
        detail: detail.into(),
    }
}

#[allow(clippy::cast_precision_loss)]
fn cpu_percent(
    previous: Option<&CpuSample>,
    current: &CpuSample,
) -> Result<Option<f64>, MetricsError> {
    let Some(previous) = previous else {
        return Ok(None);
    };
    let process_delta = current
        .process_ticks
        .checked_sub(previous.process_ticks)
        .ok_or_else(|| invalid_data("/proc/self/stat", "process CPU ticks moved backwards"))?;
    let system_delta = current
        .system_ticks
        .checked_sub(previous.system_ticks)
        .ok_or_else(|| invalid_data("/proc/stat", "aggregate CPU ticks moved backwards"))?;
    if system_delta == 0 {
        return Ok(None);
    }

    Ok(Some(
        process_delta as f64 / system_delta as f64 * f64::from(current.logical_cpu_count) * 100.0,
    ))
}

fn unix_time_ms() -> Result<u64, MetricsError> {
    let duration = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| MetricsError::SystemClockBeforeUnixEpoch)?;
    u64::try_from(duration.as_millis())
        .map_err(|_| MetricsError::ValueOutOfRange("Unix timestamp milliseconds"))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn parses_process_cpu_ticks_when_command_contains_parentheses() {
        let stat = "42 (worker ) name) S 1 2 3 4 5 6 7 8 9 10 120 30\n";

        assert_eq!(parse_process_stat(stat).expect("process stat"), 150);
    }

    #[test]
    fn parses_process_status_units() {
        let status = "Name:\toxiroute\nVmSize:\t2048 kB\nVmRSS:\t512 kB\nThreads:\t7\n";

        assert_eq!(
            parse_process_status(status).expect("process status"),
            ProcessStatus {
                resident_memory_bytes: 512 * 1024,
                virtual_memory_bytes: 2048 * 1024,
                thread_count: 7,
            }
        );
    }

    #[test]
    fn rejects_process_status_with_missing_metrics() {
        let error = parse_process_status("VmRSS:\t512 kB\nThreads:\t7\n")
            .expect_err("missing virtual memory must fail");

        assert!(error.to_string().contains("VmSize"));
    }

    #[test]
    fn parses_system_cpu_ticks_without_double_counting_guest_fields() {
        let stat = "cpu  1 2 3 4 5 6 7 8 100 200\ncpu0 1 2 3 4 5 6 7 8 50 100\ncpu1 0 0 0 0 0 0 0 0 0 0\nintr 12\n";

        assert_eq!(
            parse_system_stat(stat).expect("system stat"),
            CpuSample {
                process_ticks: 0,
                system_ticks: 36,
                logical_cpu_count: 2,
            }
        );
    }

    #[test]
    fn parses_load_and_memory_fixtures() {
        let load_average = parse_load_average("0.25 1.50 2.75 1/100 42\n").expect("load average");

        assert!((load_average[0] - 0.25).abs() < f64::EPSILON);
        assert!((load_average[1] - 1.5).abs() < f64::EPSILON);
        assert!((load_average[2] - 2.75).abs() < f64::EPSILON);
        assert_eq!(
            parse_memory_info("MemTotal: 4096 kB\nMemFree: 100 kB\nMemAvailable: 1024 kB\n")
                .expect("memory info"),
            MemoryInfo {
                total_memory_bytes: 4096 * 1024,
                available_memory_bytes: 1024 * 1024,
            }
        );
    }

    #[test]
    fn computes_cpu_percent_from_process_and_aggregate_ticks() {
        let previous = CpuSample {
            process_ticks: 100,
            system_ticks: 10_000,
            logical_cpu_count: 4,
        };
        let current = CpuSample {
            process_ticks: 125,
            system_ticks: 10_100,
            logical_cpu_count: 4,
        };

        assert_eq!(
            cpu_percent(Some(&previous), &current).expect("CPU percent"),
            Some(100.0)
        );
        assert_eq!(cpu_percent(None, &current).expect("first sample"), None);
    }
}
