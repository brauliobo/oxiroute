use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SegmentWindowConfig {
    target_ms: u64,
    maximum_ms: u64,
}

impl SegmentWindowConfig {
    pub(crate) fn new(target: Duration, maximum: Duration) -> Self {
        Self {
            target_ms: duration_millis(target),
            maximum_ms: duration_millis(maximum),
        }
    }

    pub(crate) fn elapsed_ms(start_timestamp_ms: u32, timestamp_ms: u32) -> u64 {
        u64::from(timestamp_ms.saturating_sub(start_timestamp_ms))
    }

    pub(crate) fn should_cut(self, start_timestamp_ms: u32, timestamp_ms: u32) -> bool {
        Self::elapsed_ms(start_timestamp_ms, timestamp_ms) >= self.target_ms.min(self.maximum_ms)
    }
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cuts_at_the_smaller_target_or_maximum() {
        let target = SegmentWindowConfig::new(Duration::from_secs(2), Duration::from_secs(10));
        assert!(!target.should_cut(100, 2_099));
        assert!(target.should_cut(100, 2_100));

        let maximum = SegmentWindowConfig::new(Duration::from_secs(10), Duration::from_secs(2));
        assert!(!maximum.should_cut(100, 2_099));
        assert!(maximum.should_cut(100, 2_100));
    }

    #[test]
    fn timestamp_rollback_has_zero_elapsed_time() {
        let window = SegmentWindowConfig::new(Duration::from_millis(1), Duration::from_secs(1));
        assert_eq!(SegmentWindowConfig::elapsed_ms(10, 9), 0);
        assert!(!window.should_cut(10, 9));
    }

    #[test]
    fn duration_conversion_saturates_at_u64_milliseconds() {
        let window = SegmentWindowConfig::new(Duration::MAX, Duration::MAX);
        assert_eq!(window.target_ms, u64::MAX);
        assert_eq!(window.maximum_ms, u64::MAX);
    }
}
