use oxiroute_config::{ConfigDraft, Protocol};

use super::{
    super::model::{
        CacheStats, Histogram, HttpStats, ListenerStats, OutcomeCount, ProxyProtocolStats,
        TcpRelayStats,
    },
    traffic::Hist,
};
use crate::{
    HttpOperationSnapshot, ListenerSnapshot, OPERATION_LATENCY_BUCKETS_MS, RuntimeSnapshot,
};

pub(super) fn listeners(runtime: &RuntimeSnapshot, config: &ConfigDraft) -> Vec<ListenerStats> {
    runtime
        .listeners
        .iter()
        .map(|listener| listener_stats(listener, config))
        .collect()
}

fn listener_stats(listener: &ListenerSnapshot, config: &ConfigDraft) -> ListenerStats {
    let configured = config
        .listeners
        .iter()
        .find(|configured| configured.name == listener.name);
    let service = configured.and_then(|configured| configured.service.as_deref());
    let relays_to_pool =
        configured.is_some_and(|c| matches!(c.protocol, Protocol::Tcp | Protocol::Udp));
    ListenerStats {
        name: listener.name.clone(),
        protocol: listener.protocol.clone(),
        bind: listener.bind.clone(),
        service: service.map(str::to_owned),
        tls_profile: configured.and_then(|configured| configured.tls_profile.clone()),
        upstream_pool: service
            .filter(|_| relays_to_pool)
            .and_then(|name| config.l4_services.iter().find(|l4| l4.name == name))
            .map(|l4| l4.upstream_pool.clone()),
        state: listener.state.into(),
        administrative_state: listener.administrative_state.into(),
        max_connections: listener.max_connections,
        active_connections: listener.active_connections,
        accepted_connections: listener.accepted_connections.into(),
        rejected_connections: listener.rejected_connections.into(),
        bytes_received: listener.bytes_received.into(),
        bytes_sent: listener.bytes_sent.into(),
        http: listener.http_operations.as_ref().map(http),
        tcp_relays: listener.tcp_relays.as_ref().map(|relays| {
            let outcomes = counts(&relays.outcomes, |o| (o.result.into(), o.count));
            TcpRelayStats {
                total: relays.outcomes.iter().map(|o| o.count).sum::<u64>().into(),
                outcomes,
                latency: histogram(&relays.latency),
            }
        }),
        proxy_protocol: listener
            .proxy_protocol
            .as_ref()
            .map(|proxy| ProxyProtocolStats {
                outcomes: counts(&proxy.outcomes, |o| (o.result.into(), o.count)),
            }),
        cache: listener.cache.as_ref().map(|cache| CacheStats {
            hits: cache.hits.into(),
            misses: cache.misses.into(),
            admissions: cache.admissions.into(),
            evictions: cache.evictions.into(),
        }),
    }
}

fn http(operations: &HttpOperationSnapshot) -> HttpStats {
    HttpStats {
        outcomes: counts(&operations.outcomes, |o| (o.result.into(), o.count)),
        total: operations
            .outcomes
            .iter()
            .map(|o| o.count)
            .sum::<u64>()
            .into(),
        latency: histogram(&operations.latency),
    }
}

fn counts<S, T>(source: &[S], read: impl Fn(&S) -> (T, u64)) -> Vec<OutcomeCount<T>> {
    source
        .iter()
        .map(|item| {
            let (result, count) = read(item);
            OutcomeCount {
                result,
                count: count.into(),
            }
        })
        .collect()
}

fn histogram(latency: &crate::LatencySnapshot) -> Histogram {
    Hist::from(latency).to_wire(OPERATION_LATENCY_BUCKETS_MS.len())
}
