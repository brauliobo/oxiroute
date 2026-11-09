#[allow(clippy::wildcard_imports)]
use super::*;
use crate::sharded::ShardedCounters;

pub(super) const HTTP_RESULT_COUNT: usize = HttpOperationResult::ALL.len();

pub(super) const TCP_RESULT_COUNT: usize = TcpRelayResult::ALL.len();

pub(super) const PROXY_PROTOCOL_RESULT_COUNT: usize = ProxyProtocolResult::ALL.len();

pub(super) const LATENCY_BUCKET_COUNT: usize = OPERATION_LATENCY_BUCKETS_MS.len() + 1;

pub(super) struct OperationMetricsState {
    http_results: ShardedCounters<HTTP_RESULT_COUNT>,
    http_latency: LatencyCounters,
    tcp_results: ShardedCounters<TCP_RESULT_COUNT>,
    tcp_latency: LatencyCounters,
    proxy_protocol_results: [AtomicU64; PROXY_PROTOCOL_RESULT_COUNT],
    cache_events: [AtomicU64; 4],
}

impl OperationMetricsState {
    pub(super) fn new() -> Self {
        Self {
            http_results: ShardedCounters::new(),
            http_latency: LatencyCounters::new(),
            tcp_results: ShardedCounters::new(),
            tcp_latency: LatencyCounters::new(),
            proxy_protocol_results: std::array::from_fn(|_| AtomicU64::new(0)),
            cache_events: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    pub(super) fn record_http_operation(
        &self,
        result: HttpOperationResult,
        duration: Duration,
    ) -> Result<(), MetricsError> {
        self.http_results.add(result.index(), 1);
        self.http_latency.record(duration)
    }

    pub(super) fn record_tcp_relay(
        &self,
        result: TcpRelayResult,
        duration: Duration,
    ) -> Result<(), MetricsError> {
        self.tcp_results.add(result.index(), 1);
        self.tcp_latency.record(duration)
    }

    pub(super) fn http_snapshot(&self) -> Option<HttpOperationSnapshot> {
        let latency = self.http_latency.snapshot();
        (latency.count > 0).then(|| HttpOperationSnapshot {
            outcomes: HttpOperationResult::ALL
                .into_iter()
                .map(|result| HttpOperationCountSnapshot {
                    result,
                    count: self.http_results.get(result.index()),
                })
                .collect(),
            latency,
        })
    }

    pub(super) fn record_cache_event(&self, event: CacheEvent) -> Result<(), MetricsError> {
        checked_atomic_add(&self.cache_events[event.index()], 1, "http.cache")
    }

    pub(super) fn record_proxy_protocol(
        &self,
        result: ProxyProtocolResult,
    ) -> Result<(), MetricsError> {
        checked_atomic_add(
            &self.proxy_protocol_results[result.index()],
            1,
            "proxy_protocol.results",
        )
    }

    pub(super) fn proxy_protocol_snapshot(&self) -> Option<ProxyProtocolSnapshot> {
        let values = ProxyProtocolResult::ALL.map(|result| {
            (
                result,
                self.proxy_protocol_results[result.index()].load(Ordering::Relaxed),
            )
        });
        values
            .iter()
            .any(|(_, count)| *count > 0)
            .then(|| ProxyProtocolSnapshot {
                outcomes: values
                    .into_iter()
                    .map(|(result, count)| ProxyProtocolCountSnapshot { result, count })
                    .collect(),
            })
    }

    pub(super) fn cache_snapshot(&self) -> Option<CacheSnapshot> {
        let values =
            CacheEvent::ALL.map(|event| self.cache_events[event.index()].load(Ordering::Relaxed));
        values
            .iter()
            .any(|value| *value > 0)
            .then_some(CacheSnapshot {
                hits: values[0],
                misses: values[1],
                admissions: values[2],
                evictions: values[3],
            })
    }

    pub(super) fn tcp_snapshot(&self) -> Option<TcpRelaySnapshot> {
        let latency = self.tcp_latency.snapshot();
        (latency.count > 0).then(|| TcpRelaySnapshot {
            outcomes: TcpRelayResult::ALL
                .into_iter()
                .map(|result| TcpRelayCountSnapshot {
                    result,
                    count: self.tcp_results.get(result.index()),
                })
                .collect(),
            latency,
        })
    }
}

pub(super) const TRANSPORT_OUTCOME_COUNT: usize = TransportOutcome::ALL.len();

pub(super) struct TransportMetricState {
    outcomes: ShardedCounters<TRANSPORT_OUTCOME_COUNT>,
    latency: LatencyCounters,
}

impl TransportMetricState {
    fn new() -> Self {
        Self {
            outcomes: ShardedCounters::new(),
            latency: LatencyCounters::new(),
        }
    }

    fn record(
        &self,
        outcome: TransportOutcome,
        duration: Option<Duration>,
    ) -> Result<(), MetricsError> {
        self.outcomes.add(outcome.index(), 1);
        duration.map_or(Ok(()), |duration| self.latency.record(duration))
    }

    fn snapshot(&self, transport: ObservedTransport) -> Option<TransportOperationSnapshot> {
        let latency = self.latency.snapshot();
        self.outcomes_present().then(|| TransportOperationSnapshot {
            transport,
            outcomes: TransportOutcome::ALL
                .into_iter()
                .map(|outcome| TransportOperationCountSnapshot {
                    outcome,
                    count: self.outcomes.get(outcome.index()),
                })
                .collect(),
            latency,
        })
    }

    fn outcomes_present(&self) -> bool {
        TransportOutcome::ALL
            .into_iter()
            .any(|outcome| self.outcomes.get(outcome.index()) > 0)
    }
}

pub(super) struct TransportOperationsState {
    metrics: [TransportMetricState; ObservedTransport::ALL.len()],
}

impl TransportOperationsState {
    pub(super) fn new() -> Self {
        Self {
            metrics: std::array::from_fn(|_| TransportMetricState::new()),
        }
    }

    pub(super) fn record(
        &self,
        transport: ObservedTransport,
        outcome: TransportOutcome,
        duration: Option<Duration>,
    ) -> Result<(), MetricsError> {
        self.metrics[transport.index()].record(outcome, duration)
    }

    pub(super) fn snapshots(&self) -> Vec<TransportOperationSnapshot> {
        ObservedTransport::ALL
            .into_iter()
            .filter_map(|transport| self.metrics[transport.index()].snapshot(transport))
            .collect()
    }
}

pub(super) fn event_transport_operations() -> &'static TransportOperationsState {
    static OPERATIONS: OnceLock<TransportOperationsState> = OnceLock::new();
    OPERATIONS.get_or_init(TransportOperationsState::new)
}

pub(crate) fn record_transport_event(transport: ObservedTransport, outcome: TransportOutcome) {
    let _ = event_transport_operations().record(transport, outcome, None);
}

pub(crate) fn transport_event_snapshots() -> Vec<TransportOperationSnapshot> {
    event_transport_operations().snapshots()
}

pub(super) fn transport_for_protocol(protocol: &str) -> ObservedTransport {
    match protocol {
        "forward_http1" | "forward_http2" | "forward_http3" => ObservedTransport::Forward,
        "http3" => ObservedTransport::H3,
        "udp" => ObservedTransport::Udp,
        "tcp" => ObservedTransport::Tcp,
        _ => ObservedTransport::Http,
    }
}

pub(super) fn checked_atomic_add(
    counter: &AtomicU64,
    value: u64,
    name: &'static str,
) -> Result<(), MetricsError> {
    counter
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(value)
        })
        .map(|_| ())
        .map_err(|_| MetricsError::CounterOverflow(name))
}

/// Latency counters of one operation kind. A sample lands in a single bucket; the cumulative buckets
/// and the sample count are summed when a snapshot is read, which keeps recording to two relaxed
/// additions on the calling thread's own cache lines.
pub(super) struct LatencyCounters {
    /// One counter per bucket, then the sum of the samples in milliseconds.
    counters: ShardedCounters<{ LATENCY_BUCKET_COUNT + 1 }>,
}

impl LatencyCounters {
    const SUM: usize = LATENCY_BUCKET_COUNT;

    pub(super) fn new() -> Self {
        Self {
            counters: ShardedCounters::new(),
        }
    }

    pub(super) fn record(&self, duration: Duration) -> Result<(), MetricsError> {
        let duration_ms = u64::try_from(duration.as_millis())
            .map_err(|_| MetricsError::ValueOutOfRange("operation duration milliseconds"))?;
        let bucket = OPERATION_LATENCY_BUCKETS_MS.partition_point(|bound| *bound < duration_ms);
        self.counters.add(bucket, 1);
        self.counters.add(Self::SUM, duration_ms);
        Ok(())
    }

    pub(super) fn snapshot(&self) -> LatencySnapshot {
        let mut count = 0;
        let buckets = (0..LATENCY_BUCKET_COUNT)
            .map(|index| {
                count += self.counters.get(index);
                LatencyBucketSnapshot {
                    upper_bound_ms: OPERATION_LATENCY_BUCKETS_MS.get(index).copied(),
                    count,
                }
            })
            .collect();
        LatencySnapshot {
            buckets,
            count,
            sum_ms: self.counters.get(Self::SUM),
        }
    }
}

pub(super) fn decrement_counter(counter: &AtomicU64) {
    let result = counter.try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        current.checked_sub(1)
    });
    debug_assert!(result.is_ok(), "active connection counter underflowed");
}

pub(super) fn add_total(
    total: &mut u64,
    value: u64,
    name: &'static str,
) -> Result<(), MetricsError> {
    *total = total
        .checked_add(value)
        .ok_or(MetricsError::CounterOverflow(name))?;
    Ok(())
}
