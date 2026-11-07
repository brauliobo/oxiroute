use super::*;
use crate::registry::lease::ManualClock;

#[test]
fn a_burst_is_allowed_then_requests_wait_for_refill() {
    let clock = Arc::new(ManualClock::new());
    let limiter = RateLimiter::new(clock.clone(), 10, 3);

    for _ in 0..3 {
        assert!(limiter.check("peer").is_ok());
    }
    let wait = limiter.check("peer").expect_err("burst spent");
    assert!(wait <= Duration::from_millis(100) && wait > Duration::ZERO);
    clock.advance(Duration::from_millis(100));
    assert!(limiter.check("peer").is_ok());
    assert!(limiter.check("peer").is_err());
}

#[test]
fn keys_do_not_share_buckets() {
    let limiter = RateLimiter::new(Arc::new(ManualClock::new()), 1, 1);

    assert!(limiter.check("a").is_ok());
    assert!(limiter.check("a").is_err());
    assert!(limiter.check("b").is_ok());
}

#[test]
fn idle_buckets_are_dropped_when_the_table_is_full() {
    let clock = Arc::new(ManualClock::new());
    let limiter = RateLimiter::new(clock.clone(), 100, 1);
    for index in 0..MAX_BUCKETS {
        limiter.check(&format!("key-{index}")).expect("first use");
    }
    clock.advance(Duration::from_secs(1));

    limiter.check("fresh").expect("new key");

    assert!(limiter.buckets.lock().expect("buckets").len() < MAX_BUCKETS);
}
