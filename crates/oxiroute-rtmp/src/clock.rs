use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn unix_time_ms() -> u64 {
    unix_time_ms_at(SystemTime::now())
}

fn unix_time_ms_at(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |duration| {
        duration.as_millis().try_into().unwrap_or(u64::MAX)
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn wall_clock_conversion_is_exact_and_saturates_before_the_epoch() {
        assert_eq!(
            unix_time_ms_at(UNIX_EPOCH + Duration::from_micros(1_234_999)),
            1_234
        );
        assert_eq!(unix_time_ms_at(UNIX_EPOCH - Duration::from_millis(1)), 0);
    }
}
