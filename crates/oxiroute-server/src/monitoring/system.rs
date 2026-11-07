#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CpuSample {
    pub(super) process_ticks: u64,
    pub(super) system_ticks: u64,
    pub(super) logical_cpu_count: u32,
}

pub(super) struct SystemSample {
    pub(super) process_status: ComponentStatus,
    pub(super) resident_memory_bytes: Option<u64>,
    pub(super) virtual_memory_bytes: Option<u64>,
    pub(super) thread_count: Option<u64>,
    pub(super) open_file_descriptors: Option<u64>,
    pub(super) host: HostSnapshot,
    pub(super) cpu: CpuSample,
}

#[cfg(target_os = "linux")]
pub(super) fn sample_system() -> Result<SystemSample, MetricsError> {
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
        process_status: ComponentStatus::healthy(),
        resident_memory_bytes: Some(status.resident_memory_bytes),
        virtual_memory_bytes: Some(status.virtual_memory_bytes),
        thread_count: Some(status.thread_count),
        open_file_descriptors: Some(count_open_file_descriptors()?),
        host: HostSnapshot {
            status: ComponentStatus::healthy(),
            load_average_1m: Some(load_average[0]),
            load_average_5m: Some(load_average[1]),
            load_average_15m: Some(load_average[2]),
            total_memory_bytes: Some(memory.total_memory_bytes),
            available_memory_bytes: Some(memory.available_memory_bytes),
        },
        cpu,
    })
}

#[cfg(not(target_os = "linux"))]
pub(super) fn sample_system() -> Result<SystemSample, MetricsError> {
    Ok(unsupported_system_sample())
}

#[cfg(any(not(target_os = "linux"), test))]
pub(super) fn unsupported_system_sample() -> SystemSample {
    #[cfg(not(target_os = "linux"))]
    let status = ComponentStatus::unsupported("platform_not_supported");
    #[cfg(target_os = "linux")]
    let status = ComponentStatus {
        state: ComponentState::Unsupported,
        reason: Some("platform_not_supported"),
    };
    SystemSample {
        process_status: status,
        resident_memory_bytes: None,
        virtual_memory_bytes: None,
        thread_count: None,
        open_file_descriptors: None,
        host: HostSnapshot {
            status,
            load_average_1m: None,
            load_average_5m: None,
            load_average_15m: None,
            total_memory_bytes: None,
            available_memory_bytes: None,
        },
        cpu: CpuSample {
            process_ticks: 0,
            system_ticks: 0,
            logical_cpu_count: 0,
        },
    }
}

#[cfg(target_os = "linux")]
pub(super) fn read_proc(path: &'static str) -> Result<String, MetricsError> {
    fs::read_to_string(path).map_err(|source| MetricsError::Io { path, source })
}

#[cfg(target_os = "linux")]
pub(super) fn count_open_file_descriptors() -> Result<u64, MetricsError> {
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
pub(super) struct ProcessStatus {
    pub(super) resident_memory_bytes: u64,
    pub(super) virtual_memory_bytes: u64,
    pub(super) thread_count: u64,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Eq, PartialEq)]
pub(super) struct MemoryInfo {
    pub(super) total_memory_bytes: u64,
    pub(super) available_memory_bytes: u64,
}

#[cfg(target_os = "linux")]
pub(super) fn parse_process_stat(input: &str) -> Result<u64, MetricsError> {
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
pub(super) fn parse_system_stat(input: &str) -> Result<CpuSample, MetricsError> {
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
pub(super) fn parse_process_status(input: &str) -> Result<ProcessStatus, MetricsError> {
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
pub(super) fn parse_load_average(input: &str) -> Result<[f64; 3], MetricsError> {
    const RESOURCE: &str = "/proc/loadavg";

    let mut fields = input.split_whitespace();
    Ok([
        parse_load_value(fields.next(), RESOURCE, "one-minute load average")?,
        parse_load_value(fields.next(), RESOURCE, "five-minute load average")?,
        parse_load_value(fields.next(), RESOURCE, "fifteen-minute load average")?,
    ])
}

#[cfg(target_os = "linux")]
pub(super) fn parse_memory_info(input: &str) -> Result<MemoryInfo, MetricsError> {
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
pub(super) fn set_once<T>(
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
pub(super) fn required<T>(
    value: Option<T>,
    resource: &'static str,
    field: &'static str,
) -> Result<T, MetricsError> {
    value.ok_or_else(|| invalid_data(resource, format!("missing `{field}` field")))
}

#[cfg(target_os = "linux")]
pub(super) fn parse_kib(
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
pub(super) fn parse_single_u64(
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
pub(super) fn parse_u64(
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
pub(super) fn parse_load_value(
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

pub(super) fn invalid_data(resource: &'static str, detail: impl Into<String>) -> MetricsError {
    MetricsError::InvalidData {
        resource,
        detail: detail.into(),
    }
}

#[allow(clippy::cast_precision_loss)]
pub(super) fn cpu_percent(
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

pub(super) fn unix_time_ms() -> Result<u64, MetricsError> {
    let duration = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| MetricsError::SystemClockBeforeUnixEpoch)?;
    u64::try_from(duration.as_millis())
        .map_err(|_| MetricsError::ValueOutOfRange("Unix timestamp milliseconds"))
}
