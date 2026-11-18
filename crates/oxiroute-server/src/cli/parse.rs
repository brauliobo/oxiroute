use std::time::Duration;

const DURATION_SHAPE: &str = "a duration must look like 500ms, 15s, 2m, 6h or 30d";

pub(super) fn nonempty_target(value: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        Err("target must not be empty".into())
    } else {
        Ok(value.to_owned())
    }
}

pub(super) fn parse_method(value: &str) -> Result<String, String> {
    http::Method::from_bytes(value.to_ascii_uppercase().as_bytes())
        .map(|method| method.to_string())
        .map_err(|_| format!("{value:?} is not an HTTP method"))
}

pub(super) fn parse_age_ms(value: &str) -> Result<u64, String> {
    parse_duration(value).map(|age| u64::try_from(age.as_millis()).unwrap_or(u64::MAX))
}

pub(super) fn parse_duration(value: &str) -> Result<Duration, String> {
    let (digits, unit) = value.split_at(
        value
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(value.len()),
    );
    let amount = digits
        .parse::<u64>()
        .map_err(|_| DURATION_SHAPE.to_owned())?;
    let seconds = |unit_seconds: u64| {
        amount
            .checked_mul(unit_seconds)
            .map(Duration::from_secs)
            .ok_or_else(|| DURATION_SHAPE.to_owned())
    };
    match unit {
        "ms" => Ok(Duration::from_millis(amount)),
        "s" => seconds(1),
        "m" => seconds(60),
        "h" => seconds(3_600),
        "d" => seconds(86_400),
        _ => Err(DURATION_SHAPE.into()),
    }
}

pub(super) fn parse_timeout(value: &str) -> Result<Duration, String> {
    parse_duration(value).and_then(|limit| {
        (!limit.is_zero())
            .then_some(limit)
            .ok_or("a timeout must not be zero".to_owned())
    })
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
    fn durations_take_milliseconds_through_days() {
        assert_eq!(parse_duration("500ms"), Ok(Duration::from_millis(500)));
        assert_eq!(parse_duration("2m"), Ok(Duration::from_mins(2)));
        assert_eq!(parse_duration("6h"), Ok(Duration::from_hours(6)));
        assert_eq!(parse_duration("30d"), Ok(Duration::from_hours(720)));
        assert!(parse_duration("soon").is_err());
        assert!(parse_duration("18446744073709551615d").is_err());
    }

    #[test]
    fn a_timeout_is_a_positive_duration() {
        assert_eq!(parse_timeout("15s"), Ok(Duration::from_secs(15)));
        assert!(parse_timeout("0s").is_err());
        assert!(parse_timeout("15").is_err());
    }
}
