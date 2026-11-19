use std::{
    net::SocketAddr,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::Instant,
};

use super::{key::IpKey, protocol::Protocol};

pub(crate) trait Writer {
    fn add(counter: &AtomicU64, value: u64);
}

/// The connection is driven by one thread at a time, so a counter needs no locked read-modify-write.
pub(crate) struct Exclusive;

/// Several tasks of one connection may count at once.
pub(crate) struct Concurrent;

impl Writer for Exclusive {
    #[inline]
    fn add(counter: &AtomicU64, value: u64) {
        counter.store(
            counter.load(Ordering::Relaxed).wrapping_add(value),
            Ordering::Relaxed,
        );
    }
}

impl Writer for Concurrent {
    #[inline]
    fn add(counter: &AtomicU64, value: u64) {
        counter.fetch_add(value, Ordering::Relaxed);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TlsInfo {
    pub(crate) version: &'static str,
    pub(crate) sni: Option<String>,
    pub(crate) alpn: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct Counts {
    pub(super) requests: u64,
    pub(super) errors: u64,
    pub(super) bytes_in: u64,
    pub(super) bytes_out: u64,
}

pub(crate) struct Arrival {
    pub(crate) peer: SocketAddr,
    pub(crate) listener: Arc<str>,
    pub(crate) protocol: Protocol,
    pub(crate) now: Instant,
}

pub(crate) struct ConnStats {
    requests: AtomicU64,
    errors: AtomicU64,
    bytes_in: AtomicU64,
    bytes_out: AtomicU64,
    last_activity_ms: AtomicU64,
    protocol: AtomicU8,
    handshaking: AtomicBool,
    id: u64,
    slot: u32,
    shard: usize,
    key: IpKey,
    peer: SocketAddr,
    listener: Arc<str>,
    opened: Instant,
    tls: OnceLock<TlsInfo>,
    upstream: OnceLock<Arc<str>>,
}

impl ConnStats {
    pub(super) fn new(arrival: Arrival, key: IpKey, (shard, slot, id): (usize, u32, u64)) -> Self {
        Self {
            requests: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            bytes_in: AtomicU64::new(0),
            bytes_out: AtomicU64::new(0),
            last_activity_ms: AtomicU64::new(0),
            protocol: AtomicU8::new(arrival.protocol as u8),
            handshaking: AtomicBool::new(false),
            id,
            slot,
            shard,
            key,
            peer: arrival.peer,
            listener: arrival.listener,
            opened: arrival.now,
            tls: OnceLock::new(),
            upstream: OnceLock::new(),
        }
    }

    pub(crate) const fn id(&self) -> u64 {
        self.id
    }

    pub(super) const fn slot(&self) -> u32 {
        self.slot
    }

    pub(super) const fn shard(&self) -> usize {
        self.shard
    }

    pub(super) const fn key(&self) -> IpKey {
        self.key
    }

    pub(super) const fn peer(&self) -> SocketAddr {
        self.peer
    }

    pub(super) fn listener(&self) -> &Arc<str> {
        &self.listener
    }

    pub(super) const fn opened(&self) -> Instant {
        self.opened
    }

    pub(super) fn protocol(&self) -> Protocol {
        Protocol::from_u8(self.protocol.load(Ordering::Relaxed))
    }

    pub(crate) fn set_protocol(&self, protocol: Protocol) {
        self.protocol.store(protocol as u8, Ordering::Relaxed);
    }

    pub(super) fn is_handshaking(&self) -> bool {
        self.handshaking.load(Ordering::Relaxed)
    }

    pub(crate) fn set_handshaking(&self, handshaking: bool) {
        self.handshaking.store(handshaking, Ordering::Relaxed);
    }

    pub(crate) fn set_tls(&self, tls: TlsInfo) {
        let _ = self.tls.set(tls);
    }

    pub(super) fn tls(&self) -> Option<&TlsInfo> {
        self.tls.get()
    }

    pub(crate) fn set_upstream(&self, upstream: Arc<str>) {
        let _ = self.upstream.set(upstream);
    }

    pub(super) fn upstream(&self) -> Option<&Arc<str>> {
        self.upstream.get()
    }

    #[inline]
    pub(crate) fn received<W: Writer>(&self, bytes: u64, now: Instant) {
        W::add(&self.bytes_in, bytes);
        self.touch(now);
    }

    #[inline]
    pub(crate) fn sent<W: Writer>(&self, bytes: u64, now: Instant) {
        W::add(&self.bytes_out, bytes);
        self.touch(now);
    }

    #[inline]
    pub(crate) fn transferred<W: Writer>(&self, received: u64, sent: u64, now: Instant) {
        if received > 0 {
            W::add(&self.bytes_in, received);
        }
        if sent > 0 {
            W::add(&self.bytes_out, sent);
        }
        self.touch(now);
    }

    #[inline]
    pub(crate) fn completed<W: Writer>(&self, failed: bool, now: Instant) {
        W::add(&self.requests, 1);
        if failed {
            W::add(&self.errors, 1);
        }
        self.touch(now);
    }

    #[inline]
    fn touch(&self, now: Instant) {
        self.last_activity_ms
            .store(self.elapsed_ms(now), Ordering::Relaxed);
    }

    pub(super) fn elapsed_ms(&self, now: Instant) -> u64 {
        u64::try_from(now.saturating_duration_since(self.opened).as_millis()).unwrap_or(u64::MAX)
    }

    pub(super) fn last_activity_ms(&self) -> u64 {
        self.last_activity_ms.load(Ordering::Relaxed)
    }

    pub(super) fn counts(&self) -> Counts {
        Counts {
            requests: self.requests.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            bytes_in: self.bytes_in.load(Ordering::Relaxed),
            bytes_out: self.bytes_out.load(Ordering::Relaxed),
        }
    }
}
