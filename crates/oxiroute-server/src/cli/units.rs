use time::{OffsetDateTime, format_description::BorrowedFormatItem, macros::format_description};

const TIMESTAMP: &[BorrowedFormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]Z");
const DECIMAL: [&str; 5] = ["", "k", "M", "G", "T"];
const BINARY: [&str; 5] = ["", "K", "M", "G", "T"];

#[allow(clippy::cast_precision_loss)]
pub(crate) const fn as_f64(value: u64) -> f64 {
    value as f64
}

fn scaled(value: f64, base: f64, suffixes: &[&str; 5]) -> String {
    let mut value = value;
    let mut step = 0;
    while value >= 1_000.0 && step + 1 < suffixes.len() {
        value /= base;
        step += 1;
    }
    match (step, value) {
        (0, _) => format!("{value:.0}"),
        (_, scaled) if scaled < 10.0 => format!("{scaled:.1}{}", suffixes[step]),
        (_, scaled) => format!("{scaled:.0}{}", suffixes[step]),
    }
}

pub(crate) fn count(value: u64) -> String {
    scaled(as_f64(value), 1_000.0, &DECIMAL)
}

pub(crate) fn rate(value: f64) -> String {
    if value > 0.0 && value < 10.0 {
        format!("{value:.1}")
    } else {
        scaled(value, 1_000.0, &DECIMAL)
    }
}

pub(crate) fn bytes_rate(value: f64) -> String {
    if value > 0.0 && value < 10.0 {
        format!("{value:.1}")
    } else {
        scaled(value, 1_024.0, &BINARY)
    }
}

pub(crate) fn bytes(value: u64) -> String {
    scaled(as_f64(value), 1_024.0, &BINARY)
}

pub(crate) fn millis(value: u64) -> String {
    match value {
        0..1_000 => format!("{value}ms"),
        1_000..10_000 => format!("{:.1}s", as_f64(value) / 1_000.0),
        _ => duration(value),
    }
}

pub(crate) fn duration(value_ms: u64) -> String {
    let seconds = value_ms / 1_000;
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3_600 => format!("{}m{:02}s", seconds / 60, seconds % 60),
        3_600..86_400 => format!("{}h{:02}m", seconds / 3_600, seconds % 3_600 / 60),
        _ => format!("{}d{:02}h", seconds / 86_400, seconds % 86_400 / 3_600),
    }
}

pub(crate) fn percent(numerator: u64, denominator: u64) -> Option<f64> {
    (denominator > 0).then(|| as_f64(numerator) * 100.0 / as_f64(denominator))
}

pub(crate) fn timestamp(unix_ms: u64) -> String {
    i128::from(unix_ms)
        .checked_mul(1_000_000)
        .and_then(|nanos| OffsetDateTime::from_unix_timestamp_nanos(nanos).ok())
        .and_then(|moment| moment.format(TIMESTAMP).ok())
        .unwrap_or_else(|| unix_ms.to_string())
}

#[cfg(test)]
mod tests;
