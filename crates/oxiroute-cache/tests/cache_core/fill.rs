use std::sync::Arc;

use bytes::Bytes;
use http::{HeaderMap, Method, StatusCode};
use oxiroute_cache::{BaseKey, Cache, CacheError, FillJoin, FillOutcome, Lookup, StoreOutcome};

use crate::common::{ManualClock, cache, config, request, response, timing};

#[test]
fn obsolete_generation_loses_without_replacing_the_current_fill() {
    let (cache, _) = cache();
    let request_headers = HeaderMap::new();
    let headers = response("max-age=60");
    let old = cache
        .prepare(
            request(&Method::GET, "/generation-outcome", &request_headers),
            StatusCode::OK,
            &headers,
            Bytes::from_static(b"old"),
            timing(0),
            &[],
        )
        .expect("old entry");
    let old_leader = match cache
        .begin_fill(old.key().base().clone())
        .expect("old fill")
    {
        FillJoin::Leader(leader) => leader,
        FillJoin::Follower(_) | FillJoin::AtCapacity => panic!("old leader"),
    };
    assert_eq!(cache.purge_exact(old.key()).fills_cancelled, 1);
    let new = cache
        .prepare(
            request(&Method::GET, "/generation-outcome", &request_headers),
            StatusCode::OK,
            &headers,
            Bytes::from_static(b"new"),
            timing(0),
            &[],
        )
        .expect("new entry");
    let new_leader = match cache
        .begin_fill(new.key().base().clone())
        .expect("new fill")
    {
        FillJoin::Leader(leader) => leader,
        FillJoin::Follower(_) | FillJoin::AtCapacity => panic!("new leader"),
    };

    assert_eq!(
        old_leader.store(old).expect("obsolete completion"),
        StoreOutcome::GenerationLost
    );
    assert_eq!(
        new_leader.store(new).expect("current completion"),
        StoreOutcome::Stored { evicted: 0 }
    );
    assert!(matches!(
        cache.lookup(request(
            &Method::GET,
            "/generation-outcome",
            &request_headers
        )),
        Ok(Lookup::Hit { response, .. }) if response.body == Bytes::from_static(b"new")
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn collapsed_forwarding_wakes_all_followers_after_lock_free_fill() {
    let (cache, _) = cache();
    let request_headers = HeaderMap::new();
    let entry = cache
        .prepare(
            request(&Method::GET, "/collapse", &request_headers),
            StatusCode::OK,
            &response("max-age=60"),
            Bytes::from_static(b"one"),
            timing(0),
            &[],
        )
        .expect("entry");
    let leader = match cache.begin_fill(entry.key().base().clone()).expect("fill") {
        FillJoin::Leader(leader) => leader,
        FillJoin::Follower(_) | FillJoin::AtCapacity => panic!("leader"),
    };
    let mut followers = Vec::new();
    for _ in 0..32 {
        match cache.begin_fill(entry.key().base().clone()).expect("fill") {
            FillJoin::Follower(waiter) => followers.push(tokio::spawn(waiter.wait())),
            FillJoin::Leader(_) | FillJoin::AtCapacity => panic!("follower"),
        }
    }
    assert_eq!(cache.stats().in_flight, 1);
    leader.store(entry).expect("store");
    for follower in followers {
        assert_eq!(follower.await.expect("join"), FillOutcome::Stored);
    }
    assert_eq!(cache.stats().fill_followers, 32);
}

#[tokio::test(flavor = "current_thread")]
async fn purge_and_drop_are_generation_safe_and_wake_followers() {
    let (cache, _) = cache();
    let request_headers = HeaderMap::new();
    let first = cache
        .prepare(
            request(&Method::GET, "/generation", &request_headers),
            StatusCode::OK,
            &response("max-age=60"),
            Bytes::from_static(b"old"),
            timing(0),
            &[],
        )
        .expect("old entry");
    let old_leader = match cache.begin_fill(first.key().base().clone()).expect("fill") {
        FillJoin::Leader(leader) => leader,
        FillJoin::Follower(_) | FillJoin::AtCapacity => panic!("old leader"),
    };
    let purged_waiter = match cache.begin_fill(first.key().base().clone()).expect("fill") {
        FillJoin::Follower(waiter) => waiter,
        FillJoin::Leader(_) | FillJoin::AtCapacity => panic!("old follower"),
    };
    assert_eq!(cache.purge_exact(first.key()).fills_cancelled, 1);
    assert_eq!(purged_waiter.wait().await, FillOutcome::Purged);

    let new_leader = match cache.begin_fill(first.key().base().clone()).expect("fill") {
        FillJoin::Leader(leader) => leader,
        FillJoin::Follower(_) | FillJoin::AtCapacity => panic!("new leader"),
    };
    assert_eq!(
        old_leader.store(first.clone()).expect("old completion"),
        StoreOutcome::GenerationLost
    );
    new_leader.store(first).expect("new completion");

    let cancelled_leader = match cache
        .begin_fill(
            BaseKey::new(request(&Method::GET, "/cancel", &request_headers), 1024).expect("base"),
        )
        .expect("fill")
    {
        FillJoin::Leader(leader) => leader,
        FillJoin::Follower(_) | FillJoin::AtCapacity => panic!("cancel leader"),
    };
    let cancelled_waiter = match cache
        .begin_fill(
            BaseKey::new(request(&Method::GET, "/cancel", &request_headers), 1024).expect("base"),
        )
        .expect("fill")
    {
        FillJoin::Follower(waiter) => waiter,
        FillJoin::Leader(_) | FillJoin::AtCapacity => panic!("cancel follower"),
    };
    drop(cancelled_leader);
    assert_eq!(cancelled_waiter.wait().await, FillOutcome::Cancelled);
    assert_eq!(cache.stats().in_flight, 0);
}

#[test]
fn fill_keys_and_follower_fanout_are_bounded() {
    let mut limits = config();
    limits.max_followers_per_fill = 1;
    let cache = Cache::with_clock(limits, Arc::new(ManualClock::default())).expect("cache");
    let request_headers = HeaderMap::new();
    let base = BaseKey::new(request(&Method::GET, "/fill", &request_headers), 1024).expect("base");
    let leader = match cache.begin_fill(base.clone()).expect("fill") {
        FillJoin::Leader(leader) => leader,
        FillJoin::Follower(_) | FillJoin::AtCapacity => panic!("leader"),
    };
    assert!(matches!(
        cache.begin_fill(base.clone()),
        Ok(FillJoin::Follower(_))
    ));
    assert!(matches!(cache.begin_fill(base), Ok(FillJoin::AtCapacity)));

    let long_path = format!("/{}", "x".repeat(2048));
    let oversized = BaseKey::new(request(&Method::GET, &long_path, &request_headers), 4096)
        .expect("externally oversized base");
    assert!(matches!(
        cache.begin_fill(oversized),
        Err(CacheError::InvalidFillKey)
    ));
    drop(leader);
}
