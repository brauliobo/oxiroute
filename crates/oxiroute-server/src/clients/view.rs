use std::{
    collections::HashMap,
    net::IpAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use super::{
    fold::{Folded, Totals},
    key::IpKey,
    protocol::Protocol,
    registry::{Registry, Settings},
    stats::{ConnStats, TlsInfo},
};
use crate::sharded::SHARDS;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Sort {
    Bytes,
    Connections,
    Requests,
    Recent,
}

pub(crate) struct ClientQuery {
    pub(crate) limit: usize,
    pub(crate) sort: Sort,
    pub(crate) ip_prefix: Option<String>,
}

pub(crate) struct ConnectionQuery {
    pub(crate) limit: usize,
    pub(crate) ip_prefix: Option<String>,
    pub(crate) listener: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Bucket {
    pub(crate) clients: u64,
    pub(crate) connections: u64,
    pub(crate) requests: u64,
    pub(crate) bytes_in: u64,
    pub(crate) bytes_out: u64,
}

impl Bucket {
    fn add(&mut self, row: &ClientRow) {
        self.clients += 1;
        self.connections += row.connections;
        self.requests += row.requests;
        self.bytes_in += row.bytes_in;
        self.bytes_out += row.bytes_out;
    }

    fn with_evicted(mut self, evicted: Totals) -> Self {
        self.requests += evicted.counts.requests;
        self.bytes_in += evicted.counts.bytes_in;
        self.bytes_out += evicted.counts.bytes_out;
        self
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ClientRow {
    pub(crate) ip: IpAddr,
    pub(crate) connections: u64,
    pub(crate) connections_total: u64,
    pub(crate) requests: u64,
    pub(crate) bytes_in: u64,
    pub(crate) bytes_out: u64,
    pub(crate) first_seen_age: Duration,
    pub(crate) last_activity_age: Duration,
    pub(crate) listeners: Vec<Arc<str>>,
    pub(crate) protocols: Vec<&'static str>,
    pub(crate) errors: u64,
}

pub(crate) struct ClientsView {
    pub(crate) settings: Settings,
    pub(crate) tracked_clients: u64,
    pub(crate) truncated: bool,
    pub(crate) clients: Vec<ClientRow>,
    pub(crate) other: Bucket,
    pub(crate) totals: Bucket,
}

pub(crate) struct ConnectionRow {
    pub(crate) id: u64,
    pub(crate) peer: std::net::SocketAddr,
    pub(crate) listener: Arc<str>,
    pub(crate) protocol: Protocol,
    pub(crate) tls: Option<TlsInfo>,
    pub(crate) handshaking: bool,
    pub(crate) age_ms: u64,
    pub(crate) idle_ms: u64,
    pub(crate) requests: u64,
    pub(crate) bytes_in: u64,
    pub(crate) bytes_out: u64,
    pub(crate) upstream: Option<Arc<str>>,
}

pub(crate) struct ConnectionsView {
    pub(crate) total: u64,
    pub(crate) truncated: bool,
    pub(crate) connections: Vec<ConnectionRow>,
}

struct Aggregate {
    live: u64,
    totals: Totals,
    first: Option<Instant>,
    last: Option<Instant>,
    listeners: Vec<Arc<str>>,
    protocols: u16,
}

impl Aggregate {
    fn new() -> Self {
        Self {
            live: 0,
            totals: Totals::default(),
            first: None,
            last: None,
            listeners: Vec::new(),
            protocols: 0,
        }
    }

    fn seen(&mut self, first: Instant, last: Instant) {
        self.first = Some(self.first.map_or(first, |known| known.min(first)));
        self.last = Some(self.last.map_or(last, |known| known.max(last)));
    }

    fn include_listener(&mut self, listener: &Arc<str>) {
        if !self.listeners.iter().any(|known| known == listener) {
            self.listeners.push(Arc::clone(listener));
        }
    }

    fn fold_in(&mut self, folded: &Folded) {
        self.totals = self.totals.plus(folded.totals);
        self.seen(folded.first, folded.last);
        self.protocols |= folded.protocols;
        for listener in &folded.listeners {
            self.include_listener(listener);
        }
    }

    fn live_in(&mut self, stats: &ConnStats) {
        let counts = stats.counts();
        self.live += 1;
        self.totals.connections += 1;
        self.totals.counts.requests += counts.requests;
        self.totals.counts.errors += counts.errors;
        self.totals.counts.bytes_in += counts.bytes_in;
        self.totals.counts.bytes_out += counts.bytes_out;
        self.seen(
            stats.opened(),
            stats.opened() + Duration::from_millis(stats.last_activity_ms()),
        );
        self.protocols |= stats.protocol().bit();
        self.include_listener(stats.listener());
    }

    fn row(&self, key: IpKey, now: Instant) -> ClientRow {
        ClientRow {
            ip: key.addr(),
            connections: self.live,
            connections_total: self.totals.connections,
            requests: self.totals.counts.requests,
            bytes_in: self.totals.counts.bytes_in,
            bytes_out: self.totals.counts.bytes_out,
            first_seen_age: now.saturating_duration_since(self.first.unwrap_or(now)),
            last_activity_age: now.saturating_duration_since(self.last.unwrap_or(now)),
            listeners: self.listeners.clone(),
            protocols: Protocol::names(self.protocols),
            errors: self.totals.counts.errors,
        }
    }
}

impl Registry {
    pub(crate) fn clients(&self, query: &ClientQuery, now: Instant) -> ClientsView {
        let settings = self.settings();
        let (mut aggregates, mut evicted) = self.aggregates();
        let capacity = usize::try_from(settings.max_clients).unwrap_or(usize::MAX);
        if aggregates.len() > capacity {
            evicted = evicted.plus(Self::shed(&mut aggregates, capacity));
        }
        let mut rows = aggregates
            .iter()
            .map(|(key, aggregate)| aggregate.row(*key, now))
            .collect::<Vec<_>>();
        let mut totals = Bucket::default();
        rows.iter().for_each(|row| totals.add(row));
        totals = totals.with_evicted(evicted);
        let tracked_clients = rows.len() as u64;
        if let Some(prefix) = &query.ip_prefix {
            rows.retain(|row| row.ip.to_string().starts_with(prefix.as_str()));
        }
        rows.sort_by(|left, right| {
            Self::rank(query.sort, right)
                .cmp(&Self::rank(query.sort, left))
                .then_with(|| left.ip.cmp(&right.ip))
        });
        let truncated = rows.len() > query.limit;
        let mut other = Bucket::default();
        rows[query.limit.min(rows.len())..]
            .iter()
            .for_each(|row| other.add(row));
        rows.truncate(query.limit);
        if query.ip_prefix.is_none() {
            other = other.with_evicted(evicted);
        }
        ClientsView {
            settings,
            tracked_clients,
            truncated,
            clients: rows,
            other,
            totals,
        }
    }

    fn rank(sort: Sort, row: &ClientRow) -> u128 {
        match sort {
            Sort::Bytes => u128::from(row.bytes_in) + u128::from(row.bytes_out),
            Sort::Connections => u128::from(row.connections),
            Sort::Requests => u128::from(row.requests),
            Sort::Recent => u128::MAX - row.last_activity_age.as_nanos(),
        }
    }

    fn aggregates(&self) -> (HashMap<IpKey, Aggregate>, Totals) {
        let mut aggregates = HashMap::<IpKey, Aggregate>::new();
        let mut evicted = Totals::default();
        let mut live = Vec::new();
        for index in 0..SHARDS {
            let shard = self.lock(index);
            for (key, folded) in shard.history.clients() {
                aggregates
                    .entry(key)
                    .or_insert_with(Aggregate::new)
                    .fold_in(folded);
            }
            evicted = evicted.plus(shard.history.evicted());
            live.extend(shard.live().cloned());
        }
        for stats in &live {
            aggregates
                .entry(stats.key())
                .or_insert_with(Aggregate::new)
                .live_in(stats);
        }
        (aggregates, evicted)
    }

    fn shed(aggregates: &mut HashMap<IpKey, Aggregate>, capacity: usize) -> Totals {
        let mut by_recency = aggregates
            .iter()
            .map(|(key, aggregate)| (aggregate.live > 0, aggregate.last, *key))
            .collect::<Vec<_>>();
        by_recency.sort_by_key(|(live, last, _)| (*live, *last));
        let mut shed = Totals::default();
        for (_, _, key) in by_recency.into_iter().take(aggregates.len() - capacity) {
            if let Some(aggregate) = aggregates.remove(&key) {
                shed = shed.plus(aggregate.totals);
            }
        }
        shed
    }

    pub(crate) fn connections(&self, query: &ConnectionQuery, now: Instant) -> ConnectionsView {
        let mut matching = Vec::new();
        for index in 0..SHARDS {
            let shard = self.lock(index);
            matching.extend(
                shard
                    .live()
                    .filter(|stats| Self::wanted(query, stats))
                    .cloned(),
            );
        }
        matching.sort_by_key(|stats| (stats.opened(), stats.id()));
        let total = matching.len() as u64;
        matching.truncate(query.limit);
        ConnectionsView {
            total,
            truncated: total > matching.len() as u64,
            connections: matching.iter().map(|stats| Self::row(stats, now)).collect(),
        }
    }

    fn wanted(query: &ConnectionQuery, stats: &ConnStats) -> bool {
        query
            .listener
            .as_deref()
            .is_none_or(|listener| listener == stats.listener().as_ref())
            && query.ip_prefix.as_deref().is_none_or(|prefix| {
                stats
                    .peer()
                    .ip()
                    .to_canonical()
                    .to_string()
                    .starts_with(prefix)
            })
    }

    fn row(stats: &ConnStats, now: Instant) -> ConnectionRow {
        let counts = stats.counts();
        let age_ms = stats.elapsed_ms(now);
        ConnectionRow {
            id: stats.id(),
            peer: stats.peer(),
            listener: Arc::clone(stats.listener()),
            protocol: stats.protocol(),
            tls: stats.tls().cloned(),
            handshaking: stats.is_handshaking(),
            age_ms,
            idle_ms: age_ms.saturating_sub(stats.last_activity_ms()),
            requests: counts.requests,
            bytes_in: counts.bytes_in,
            bytes_out: counts.bytes_out,
            upstream: stats.upstream().cloned(),
        }
    }
}
