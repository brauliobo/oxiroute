use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

pub const MAX_RENEWAL_INFORMATION_WINDOW_SECONDS: u64 = 366 * 24 * 60 * 60;

/// Supplies Unix seconds to the lifecycle without coupling it to wall-clock reads.
pub trait Clock: Send + Sync {
    fn now_unix_seconds(&self) -> u64;

    fn sleep_seconds(&self, seconds: u64) {
        std::thread::sleep(std::time::Duration::from_secs(seconds));
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix_seconds(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map_or(0, |duration| duration.as_secs())
    }
}

#[derive(Clone, Debug)]
pub struct FakeClock {
    now: Arc<AtomicU64>,
}

impl FakeClock {
    #[must_use]
    pub fn new(now_unix_seconds: u64) -> Self {
        Self {
            now: Arc::new(AtomicU64::new(now_unix_seconds)),
        }
    }

    pub fn set(&self, now_unix_seconds: u64) {
        self.now.store(now_unix_seconds, Ordering::Release);
    }

    pub fn advance(&self, seconds: u64) {
        self.now.fetch_add(seconds, Ordering::AcqRel);
    }
}

impl Clock for FakeClock {
    fn now_unix_seconds(&self) -> u64 {
        self.now.load(Ordering::Acquire)
    }

    fn sleep_seconds(&self, seconds: u64) {
        self.advance(seconds);
    }
}

/// Returns whether a certificate is inside its bounded renewal window.
#[must_use]
pub fn renewal_due(
    now_unix_seconds: u64,
    not_before_unix_seconds: u64,
    not_after_unix_seconds: u64,
    suggested_renewal_unix_seconds: Option<u64>,
) -> bool {
    if not_after_unix_seconds <= not_before_unix_seconds {
        return true;
    }
    if suggested_renewal_unix_seconds.is_some_and(|time| now_unix_seconds >= time) {
        return true;
    }

    let lifetime = not_after_unix_seconds - not_before_unix_seconds;
    let remaining = not_after_unix_seconds.saturating_sub(now_unix_seconds);
    let divisor = if lifetime < 10 * 24 * 60 * 60 { 2 } else { 3 };
    remaining <= lifetime / divisor
}

/// Picks a deterministic time in the renewal window from stable certificate identity bytes.
#[must_use]
pub fn stable_renewal_time(
    not_before_unix_seconds: u64,
    not_after_unix_seconds: u64,
    stable_identity: &str,
) -> Option<u64> {
    if not_after_unix_seconds <= not_before_unix_seconds {
        return None;
    }
    let lifetime = not_after_unix_seconds - not_before_unix_seconds;
    let divisor = if lifetime < 10 * 24 * 60 * 60 { 2 } else { 3 };
    let window = lifetime / divisor;
    let start = not_after_unix_seconds.saturating_sub(window);
    let digest = sha256(stable_identity.as_bytes());
    let mut prefix = [0_u8; 8];
    prefix.copy_from_slice(digest.get(..8).unwrap_or_default());
    let offset = u64::from_be_bytes(prefix) % (window + 1);
    Some(start.saturating_add(offset))
}

/// Picks a deterministic time inside a bounded ARI window and the certificate validity interval.
#[must_use]
pub fn stable_renewal_time_in_window(
    window_start_unix_seconds: u64,
    window_end_unix_seconds: u64,
    not_before_unix_seconds: u64,
    not_after_unix_seconds: u64,
    stable_identity: &str,
) -> Option<u64> {
    if window_end_unix_seconds <= window_start_unix_seconds
        || window_end_unix_seconds.saturating_sub(window_start_unix_seconds)
            > MAX_RENEWAL_INFORMATION_WINDOW_SECONDS
        || not_after_unix_seconds <= not_before_unix_seconds.saturating_add(1)
    {
        return None;
    }
    let start = window_start_unix_seconds.max(not_before_unix_seconds);
    let end = window_end_unix_seconds.min(not_after_unix_seconds.saturating_sub(1));
    if end < start {
        return None;
    }
    let digest = sha256(format!("ari:{stable_identity}").as_bytes());
    let mut prefix = [0_u8; 8];
    prefix.copy_from_slice(digest.get(..8).unwrap_or_default());
    let offset = u64::from_be_bytes(prefix) % (end - start + 1);
    Some(start + offset)
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest as _, Sha256};

    Sha256::digest(bytes).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_clock_is_deterministic_and_shared() {
        let clock = FakeClock::new(100);
        let clone = clock.clone();
        clone.advance(25);
        assert_eq!(clock.now_unix_seconds(), 125);
        clock.set(200);
        assert_eq!(clone.now_unix_seconds(), 200);
    }

    #[test]
    fn renewal_policy_uses_short_lived_certificate_window() {
        let before = 1_000;
        let after = before + 9 * 24 * 60 * 60;
        assert!(!renewal_due(before + 3 * 24 * 60 * 60, before, after, None));
        assert!(renewal_due(before + 5 * 24 * 60 * 60, before, after, None));
    }

    #[test]
    fn stable_renewal_time_is_reproducible_and_bounded() {
        let first = stable_renewal_time(100, 100 + 30 * 24 * 60 * 60, "edge.example");
        let second = stable_renewal_time(100, 100 + 30 * 24 * 60 * 60, "edge.example");
        assert_eq!(first, second);
        assert!(first.is_some_and(|value| {
            (100 + 20 * 24 * 60 * 60..=100 + 30 * 24 * 60 * 60).contains(&value)
        }));
    }

    #[test]
    fn ari_renewal_time_is_bounded_by_the_window_and_certificate() {
        let start = 1_000;
        let end = 2_000;
        let selected = stable_renewal_time_in_window(start, end, 1_500, 1_750, "edge.example");
        assert!(selected.is_some_and(|value| (1_500..=1_749).contains(&value)));
        assert_eq!(
            stable_renewal_time_in_window(
                start,
                start + MAX_RENEWAL_INFORMATION_WINDOW_SECONDS + 1,
                1,
                10_000,
                "edge.example"
            ),
            None
        );
    }
}
