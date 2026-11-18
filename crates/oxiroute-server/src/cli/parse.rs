use std::time::Duration;

pub(super) fn nonempty_target(value: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        Err("target must not be empty".into())
    } else {
        Ok(value.to_owned())
    }
}

pub(super) fn parse_age_ms(value: &str) -> Result<u64, String> {
    let (digits, unit_ms) = [("ms", 1), ("s", 1_000), ("m", 60_000), ("h", 3_600_000)]
        .into_iter()
        .find_map(|(suffix, unit)| value.strip_suffix(suffix).map(|digits| (digits, unit)))
        .ok_or("expected a number with a unit: ms, s, m or h")?;
    digits
        .parse::<u64>()
        .ok()
        .and_then(|count| count.checked_mul(unit_ms))
        .ok_or_else(|| "age is not a valid number".to_owned())
}

pub(super) fn parse_ttl(value: &str) -> Result<Duration, String> {
    let (digits, unit) = value.split_at(
        value
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(value.len()),
    );
    let amount = digits
        .parse::<u64>()
        .map_err(|_| "ttl must look like 500ms, 15s or 2m".to_owned())?;
    match unit {
        "ms" => Ok(Duration::from_millis(amount)),
        "s" => Ok(Duration::from_secs(amount)),
        "m" => Ok(Duration::from_secs(amount.saturating_mul(60))),
        _ => Err("ttl must look like 500ms, 15s or 2m".into()),
    }
}

pub(super) fn parse_label(value: &str) -> Result<(String, String), String> {
    value
        .split_once('=')
        .filter(|(key, _)| !key.is_empty())
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .ok_or_else(|| "label must be KEY=VALUE".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ages_need_a_unit() {
        assert_eq!(parse_age_ms("500ms"), Ok(500));
        assert_eq!(parse_age_ms("2m"), Ok(120_000));
        assert_eq!(parse_age_ms("1h"), Ok(3_600_000));
        assert!(parse_age_ms("30").is_err());
        assert!(parse_age_ms("xs").is_err());
    }

    #[test]
    fn ttls_take_milliseconds_seconds_and_minutes() {
        assert_eq!(parse_ttl("500ms"), Ok(Duration::from_millis(500)));
        assert_eq!(parse_ttl("2m"), Ok(Duration::from_mins(2)));
        assert!(parse_ttl("soon").is_err());
    }
}
