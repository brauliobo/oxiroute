use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use oxiroute_cache::{Cache, CacheConfig, CacheResponse, CacheTimeline, Clock, MonoTime};

use super::*;
use crate::RuntimeMetrics;

#[derive(Default)]
struct ManualClock(AtomicU64);

impl ManualClock {
    fn set(&self, seconds: u64) {
        self.0.store(seconds, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> MonoTime {
        MonoTime::from_duration(Duration::from_secs(self.0.load(Ordering::SeqCst)))
    }
}

struct Fixture {
    clock: Arc<ManualClock>,
    metrics: RuntimeMetrics,
    listener: ListenerMetrics,
    plan: Arc<HttpCachePlan>,
}

impl Fixture {
    fn new(mut config: CacheConfig) -> Self {
        config.max_total_bytes = config.max_total_bytes.max(64 * 1024);
        let clock = Arc::new(ManualClock::default());
        let cache = Cache::with_clock(config, clock.clone()).expect("cache");
        let metrics = RuntimeMetrics::new();
        let listener = metrics
            .register_listener("cache-kernel", "http", "127.0.0.1:8080", None)
            .expect("listener");
        let plan = Arc::new(HttpCachePlan {
            cache: Arc::new(HttpCacheBackend::Memory(Arc::new(cache))),
            timeline: CacheTimeline::new(
                true,
                Duration::from_mins(1),
                [],
                Duration::from_mins(1),
                Duration::from_mins(2),
            )
            .expect("timeline"),
            methods: vec![Method::GET, Method::HEAD].into_boxed_slice(),
            revalidate: true,
            surrogate_header: None,
            surrogate_limits: None,
            purge_access: None,
        });
        Self {
            clock,
            metrics,
            listener,
            plan,
        }
    }

    fn transaction(&self, path: &str, headers: HeaderMap) -> CacheTransaction {
        let request = CacheRequest {
            method: Method::GET,
            scheme: "https",
            authority: "example.test".to_owned(),
            path: path.to_owned(),
            query: None,
            headers,
            request_started: self.plan.cache.now(),
        };
        CacheTransaction::new(Arc::clone(&self.plan), request, self.listener.clone())
    }

    fn cache_metrics(&self) -> crate::CacheSnapshot {
        self.metrics
            .snapshot()
            .expect("snapshot")
            .listeners
            .into_iter()
            .find(|listener| listener.name == "cache-kernel")
            .and_then(|listener| listener.cache)
            .expect("cache metrics")
    }
}

fn config() -> CacheConfig {
    CacheConfig {
        max_entries: 8,
        max_total_bytes: 64 * 1024,
        max_object_bytes: 8 * 1024,
        max_header_bytes: 2 * 1024,
        max_header_fields: 32,
        max_body_bytes: 4 * 1024,
        max_key_bytes: 1024,
        max_vary_fields: 8,
        max_tags_per_entry: 4,
        max_tag_bytes: 32,
        max_in_flight: 8,
        max_followers_per_fill: 8,
        max_heuristic_freshness: Duration::from_hours(24),
    }
}

fn response_headers(cache_control: &'static str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control),
    );
    headers.insert(http::header::ETAG, HeaderValue::from_static("\"kernel\""));
    headers
}

async fn admit_response(
    transaction: &mut CacheTransaction,
    cache_control: &'static str,
    body: &'static [u8],
) -> CacheAdmission {
    let headers = response_headers(cache_control);
    let timing = ResponseTiming {
        request_started: transaction.request.request_started,
        response_received: transaction.plan.cache.now(),
        response_received_wall: SystemTime::now(),
    };
    let entry = transaction
        .plan
        .cache
        .prepare_with_timeline(
            transaction.request.representation_input(),
            CacheResponse {
                status: StatusCode::OK,
                headers: &headers,
                body: Bytes::from_static(body),
                timing,
                tags: &[],
            },
            &transaction.plan.timeline,
        )
        .expect("prepared response");
    transaction.admit(entry).await.expect("admission")
}

async fn seed(fixture: &Fixture, path: &str, cache_control: &'static str) -> CacheAdmission {
    let CacheStart::MissLeader(mut transaction) =
        fixture.transaction(path, HeaderMap::new()).start().await
    else {
        panic!("initial request must lead the fill");
    };
    admit_response(&mut transaction, cache_control, b"cached").await
}

#[tokio::test]
async fn transaction_trace_classifies_bypass_hit_and_only_if_cached() {
    let fixture = Fixture::new(config());
    let mut bypass_headers = HeaderMap::new();
    bypass_headers.insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    assert!(matches!(
        fixture.transaction("/bypass", bypass_headers).start().await,
        CacheStart::Bypass(CacheStartFailure::Policy)
    ));

    assert_eq!(
        seed(&fixture, "/hit", "public, max-age=60").await,
        CacheAdmission::Stored { evicted: 0 }
    );
    assert!(matches!(
        fixture
            .transaction("/hit", HeaderMap::new())
            .start()
            .await,
        CacheStart::Hit(response) if response.body == Bytes::from_static(b"cached")
    ));

    let mut only_if_cached = HeaderMap::new();
    only_if_cached.insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("only-if-cached"),
    );
    assert!(matches!(
        fixture
            .transaction("/uncached", only_if_cached)
            .start()
            .await,
        CacheStart::OnlyIfCached
    ));
}

#[tokio::test]
async fn transaction_trace_records_miss_leader_follower_and_cancellation() {
    let fixture = Fixture::new(config());
    let CacheStart::MissLeader(mut leader) = fixture
        .transaction("/collapsed", HeaderMap::new())
        .start()
        .await
    else {
        panic!("first request must lead");
    };
    assert_eq!(
        leader.trace,
        [CacheTraceEvent::LookupMiss, CacheTraceEvent::FillLeader]
    );

    let follower = fixture.transaction("/collapsed", HeaderMap::new());
    let follower_task = tokio::spawn(follower.start());
    tokio::task::yield_now().await;
    leader.cancel();

    let CacheStart::MissLeader(mut replacement) = follower_task.await.expect("follower task")
    else {
        panic!("cancelled fill must release replacement leadership");
    };
    assert_eq!(
        replacement.trace,
        [
            CacheTraceEvent::LookupMiss,
            CacheTraceEvent::FillFollower,
            CacheTraceEvent::FollowerStopped(FillOutcome::Cancelled),
            CacheTraceEvent::LookupMiss,
            CacheTraceEvent::FillLeader,
        ]
    );
    replacement.complete_without_store();
    assert_eq!(replacement.state, CacheTransactionState::Completed);
}

#[tokio::test]
async fn dropping_leader_transaction_cancels_fill_and_wakes_follower() {
    let fixture = Fixture::new(config());
    let CacheStart::MissLeader(leader) = fixture
        .transaction("/dropped", HeaderMap::new())
        .start()
        .await
    else {
        panic!("first request must lead");
    };
    let follower = tokio::spawn(fixture.transaction("/dropped", HeaderMap::new()).start());
    tokio::task::yield_now().await;
    drop(leader);

    let CacheStart::MissLeader(mut replacement) = follower.await.expect("follower task") else {
        panic!("dropped fill must release replacement leadership");
    };
    replacement.complete_without_store();
}

#[tokio::test]
async fn transaction_trace_bounds_followers_and_reports_fill_capacity() {
    let mut limits = config();
    limits.max_in_flight = 1;
    let fixture = Fixture::new(limits);
    let CacheStart::MissLeader(mut leader) = fixture
        .transaction("/leader", HeaderMap::new())
        .start()
        .await
    else {
        panic!("first request must lead");
    };
    assert!(matches!(
        fixture
            .transaction("/capacity", HeaderMap::new())
            .start()
            .await,
        CacheStart::Bypass(CacheStartFailure::AtCapacity)
    ));

    leader.cancel();
    assert_eq!(leader.state, CacheTransactionState::Cancelled);
    assert!(follower_should_retry(1, FillOutcome::Cancelled));
    assert!(!follower_should_retry(2, FillOutcome::Cancelled));
    assert!(follower_should_retry(2, FillOutcome::Stored));
}

#[tokio::test]
async fn transaction_trace_exposes_revalidation_and_classifies_stale_failures() {
    let fixture = Fixture::new(config());
    seed(
        &fixture,
        "/revalidate",
        "public, max-age=1, stale-if-error=60",
    )
    .await;
    fixture.clock.set(2);

    let CacheStart::RevalidationLeader(mut transaction) = fixture
        .transaction("/revalidate", HeaderMap::new())
        .start()
        .await
    else {
        panic!("expired response must revalidate");
    };
    let mut validators = HeaderMap::new();
    transaction.apply_validators(&mut validators);
    assert_eq!(
        validators.get(http::header::IF_NONE_MATCH),
        Some(&HeaderValue::from_static("\"kernel\""))
    );
    assert_eq!(
        transaction.stale_eligibility(CacheFailureClass::Upstream),
        StaleEligibility::Allowed
    );
    assert_eq!(
        transaction.stale_eligibility(CacheFailureClass::Local),
        StaleEligibility::LocalFailure
    );
    assert!(
        transaction
            .stale_response(CacheFailureClass::Upstream)
            .await
            .is_some()
    );
    assert!(
        transaction
            .stale_response(CacheFailureClass::Local)
            .await
            .is_none()
    );
    transaction.complete_without_store();
}

#[tokio::test]
async fn transaction_revalidation_merges_304_metadata_and_admits_once() {
    let fixture = Fixture::new(config());
    seed(&fixture, "/not-modified", "public, max-age=1").await;
    fixture.clock.set(2);

    let CacheStart::RevalidationLeader(mut transaction) = fixture
        .transaction("/not-modified", HeaderMap::new())
        .start()
        .await
    else {
        panic!("expired response must revalidate");
    };
    let mut not_modified = HeaderMap::new();
    not_modified.insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=60"),
    );
    not_modified.insert(
        http::header::ETAG,
        HeaderValue::from_static("\"refreshed\""),
    );
    let timing = ResponseTiming {
        request_started: transaction.request().request_started,
        response_received: transaction.now(),
        response_received_wall: SystemTime::now(),
    };

    let response = transaction.finish_revalidation(&not_modified, timing).await;
    assert_eq!(response.body, Bytes::from_static(b"cached"));
    assert_eq!(
        response.headers.get(http::header::ETAG),
        Some(&HeaderValue::from_static("\"refreshed\""))
    );
    let metrics = fixture.cache_metrics();
    assert_eq!(metrics.misses, 2);
    assert_eq!(metrics.admissions, 2);
    assert_eq!(metrics.hits, 0);
}

#[tokio::test]
async fn transaction_follower_stops_after_two_unsuccessful_waits() {
    let fixture = Fixture::new(config());
    let CacheStart::MissLeader(mut first) = fixture
        .transaction("/two-waits", HeaderMap::new())
        .start()
        .await
    else {
        panic!("first request must lead");
    };
    let base = first.plan.cache.base(&first.request).expect("fill base");
    let follower = tokio::spawn(fixture.transaction("/two-waits", HeaderMap::new()).start());
    tokio::task::yield_now().await;
    first.cancel();
    let CacheFillJoin::Leader(second) = fixture
        .plan
        .cache
        .begin_fill(base)
        .await
        .expect("second fill")
    else {
        panic!("replacement fill must lead");
    };
    tokio::task::yield_now().await;
    second.complete_without_store();

    assert!(matches!(
        follower.await.expect("follower task"),
        CacheStart::Bypass(CacheStartFailure::Follower(FillOutcome::NotStored))
    ));
}

#[tokio::test]
async fn transaction_trace_reports_generation_loss_without_admission_metrics() {
    let fixture = Fixture::new(config());
    let CacheStart::MissLeader(mut transaction) = fixture
        .transaction("/generation", HeaderMap::new())
        .start()
        .await
    else {
        panic!("request must lead");
    };
    let base = transaction
        .plan
        .cache
        .base(&transaction.request)
        .expect("base key");
    transaction
        .plan
        .cache
        .purge_base(&base)
        .await
        .expect("purge");

    assert_eq!(
        admit_response(&mut transaction, "public, max-age=60", b"lost").await,
        CacheAdmission::GenerationLost
    );
    assert_eq!(
        transaction.trace.last(),
        Some(&CacheTraceEvent::GenerationLost)
    );
    assert_eq!(fixture.cache_metrics().admissions, 0);
}

#[tokio::test]
async fn transaction_trace_emits_hit_miss_admission_and_eviction_metrics() {
    let mut limits = config();
    limits.max_entries = 1;
    let fixture = Fixture::new(limits);
    seed(&fixture, "/first", "public, max-age=60").await;
    assert_eq!(
        seed(&fixture, "/second", "public, max-age=60").await,
        CacheAdmission::Stored { evicted: 1 }
    );
    assert!(matches!(
        fixture
            .transaction("/second", HeaderMap::new())
            .start()
            .await,
        CacheStart::Hit(_)
    ));

    let metrics = fixture.cache_metrics();
    assert_eq!(metrics.hits, 1);
    assert_eq!(metrics.misses, 2);
    assert_eq!(metrics.admissions, 2);
    assert_eq!(metrics.evictions, 1);
}
