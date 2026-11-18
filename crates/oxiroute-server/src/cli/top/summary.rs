use super::{rates::Sampling, units};
use crate::observability::stats_page::model::{ProcessStats, StatsSnapshot, WorkerScaling};

pub(super) fn lines(sampling: &Sampling) -> [String; 3] {
    let now = sampling.now;
    let process = &now.process;
    [identity(now), resources(process), traffic(sampling)]
}

fn identity(now: &StatsSnapshot) -> String {
    let generation = &now.generation;
    let state = if generation.accepting {
        "accepting"
    } else {
        "not accepting"
    };
    let retired = match generation.retired.len() {
        0 => String::new(),
        retired => format!(" (+{retired} draining)"),
    };
    format!(
        "oxiroute {}  up {}  generation #{} {state}{retired}  workers {}  warnings {}",
        now.process.version,
        units::duration(now.process.uptime_ms),
        generation.number,
        workers(now),
        now.warnings.len(),
    )
}

fn workers(now: &StatsSnapshot) -> String {
    match now.workers.scaling {
        WorkerScaling::Static => "static".to_owned(),
        WorkerScaling::Dynamic => {
            let (active, total) = now
                .workers
                .runtimes
                .iter()
                .fold((0, 0), |(active, total), runtime| {
                    (active + runtime.active, total + runtime.workers)
                });
            format!("dynamic {active}/{total} active")
        }
    }
}

fn optional<T>(value: Option<T>, format: impl FnOnce(T) -> String) -> String {
    value.map_or_else(|| "-".to_owned(), format)
}

fn resources(process: &ProcessStats) -> String {
    let host = &process.host;
    format!(
        "cpu {}  rss {}  threads {}  fds {}  load {} {} {}  inflight {} (oldest {}, overdue {}, stuck {})",
        optional(process.cpu_percent, |cpu| format!("{cpu:.0}%")),
        optional(process.resident_memory_bytes, units::bytes),
        optional(process.thread_count, |count| count.to_string()),
        optional(process.open_file_descriptors, |count| count.to_string()),
        optional(host.load_average_1m, |load| format!("{load:.2}")),
        optional(host.load_average_5m, |load| format!("{load:.2}")),
        optional(host.load_average_15m, |load| format!("{load:.2}")),
        process.inflight.upstream_requests,
        units::millis(process.inflight.oldest_upstream_age_ms),
        process.inflight.overdue,
        process.inflight.stuck,
    )
}

fn traffic(sampling: &Sampling) -> String {
    let process = &sampling.now.process;
    let before = sampling.before.map(|before| &before.process);
    let rate = |now: u64, before: Option<u64>, format: fn(f64) -> String| {
        optional(sampling.rate(now, before), format)
    };
    let connections = match process.max_connections {
        Some(max) => format!("{}/{}", process.active_connections, units::count(max)),
        None => process.active_connections.to_string(),
    };
    format!(
        "conn {connections}  accepted {}/s  rejected {}  in {}B/s  out {}B/s  retries {}",
        rate(
            process.accepted_connections.get(),
            before.map(|p| p.accepted_connections.get()),
            units::rate
        ),
        units::count(process.rejected_connections.get()),
        rate(
            process.bytes_received.get(),
            before.map(|p| p.bytes_received.get()),
            units::bytes_rate
        ),
        rate(
            process.bytes_sent.get(),
            before.map(|p| p.bytes_sent.get()),
            units::bytes_rate
        ),
        units::count(process.retry_attempts.get()),
    )
}
