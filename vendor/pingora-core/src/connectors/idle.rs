// Copyright 2026 Cloudflare, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Idle upstream connections for reuse, without a task per connection.
//!
//! The upstream pool used to spawn a task for every released connection that read the idle socket
//! and waited for the next checkout, which added two scheduler round trips and about ten
//! allocations to every proxied request. Here a released connection is polled once with a waker
//! that records the connection in a shared queue, so a peer that closes (or sends data) wakes one
//! pool-wide task that drops the connection promptly. A checkout only pops an entry.
//!
//! Entries live in per-thread stripes: a worker normally takes back what it released and never
//! touches another worker's lock. Capacity is global (the keepalive size of the server configuration
//! times its threads), as in the upstream pool; a release at capacity evicts the connection released
//! longest ago in its own stripe, or in the next stripe that holds one.

use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::{AsyncRead, ReadBuf};
use tokio::runtime;
use tokio::sync::Notify;
use tokio::time::{sleep_until, Instant};

use crate::protocols::Stream;

/// One stripe per CPU, and at least eight: the workers of a listener (up to as many as there are CPUs) do not share a
/// stripe and its lock.
fn stripe_count() -> usize {
    static COUNT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *COUNT.get_or_init(|| std::thread::available_parallelism().map_or(8, usize::from).clamp(8, 256))
}

static NEXT_STRIPE: AtomicUsize = AtomicUsize::new(0);

std::thread_local! {
    static STRIPE: usize = NEXT_STRIPE.fetch_add(1, Ordering::Relaxed) % stripe_count();
}

/// Keys are already hashes.
#[derive(Default)]
struct KeyHasher(u64);

impl Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = (self.0 << 8) | u64::from(*byte);
        }
    }

    fn write_u64(&mut self, value: u64) {
        self.0 = value;
    }
}

struct Idle {
    id: i32,
    sequence: u64,
    released: Instant,
    expires: Option<Instant>,
    /// The runtime whose I/O driver the socket is registered with.
    runtime: runtime::Id,
    stream: Stream,
}

impl Idle {
    /// The stream, registered with the I/O driver of the runtime that takes it, so that its readiness events arrive on
    /// the thread that uses it and do not wake the one that released it (a runtime per worker: the connections of a
    /// released worker would keep waking it). `None` when the socket could not be registered again.
    fn into_current_runtime(self) -> Option<Stream> {
        let mut stream = self.stream;
        if self.runtime != runtime::Handle::current().id() && stream.can_move_to_current_runtime() {
            stream.move_to_current_runtime().ok()?;
        }
        Some(stream)
    }
}

#[derive(Default)]
struct Stripe {
    by_key: HashMap<u64, VecDeque<Idle>, BuildHasherDefault<KeyHasher>>,
    next_sequence: u64,
}

impl Stripe {
    fn remove_oldest(&mut self) -> Option<Idle> {
        let key = self
            .by_key
            .iter()
            .filter_map(|(key, idle)| Some((*key, idle.front()?.sequence)))
            .min_by_key(|(_, sequence)| *sequence)?
            .0;
        self.pop(key, VecDeque::pop_front)
    }

    fn pop(
        &mut self,
        key: u64,
        take: impl FnOnce(&mut VecDeque<Idle>) -> Option<Idle>,
    ) -> Option<Idle> {
        self.by_key.get_mut(&key).and_then(take)
    }

    fn remove_where(&mut self, mut remove: impl FnMut(&mut Idle) -> bool, removed: &mut Vec<Idle>) {
        for idle in self.by_key.values_mut() {
            let mut index = 0;
            while index < idle.len() {
                if remove(&mut idle[index]) {
                    removed.extend(idle.remove(index));
                } else {
                    index += 1;
                }
            }
        }
        self.by_key.retain(|_, idle| !idle.is_empty());
    }
}

#[derive(Default)]
struct Wakeups {
    /// Stripe and descriptor of every idle connection the kernel reported activity on.
    events: Mutex<Vec<(usize, i32)>>,
    notify: Notify,
    /// The earliest idle timeout of any pooled connection.
    deadline: Mutex<Option<Instant>>,
}

struct IdleWake {
    wakeups: Arc<Wakeups>,
    stripe: usize,
    id: i32,
}

impl Wake for IdleWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.wakeups.events.lock().push((self.stripe, self.id));
        self.wakeups.notify.notify_one();
    }
}

/// Idle connections by reuse key.
pub(crate) struct IdlePool {
    stripes: Box<[Padded<Mutex<Stripe>>]>,
    capacity: usize,
    idle: AtomicUsize,
    /// Counts connections found sending data while idle, shared with the connector.
    unexpected_data: Arc<AtomicU64>,
    wakeups: Arc<Wakeups>,
    watcher_started: AtomicBool,
}

/// A value alone on its cache lines.
#[repr(align(128))]
struct Padded<T>(T);

impl IdlePool {
    pub(crate) fn new(capacity: usize, unexpected_data: Arc<AtomicU64>) -> Arc<Self> {
        Arc::new(Self {
            stripes: (0..stripe_count()).map(|_| Padded(Mutex::default())).collect(),
            capacity,
            idle: AtomicUsize::new(0),
            unexpected_data,
            wakeups: Arc::default(),
            watcher_started: AtomicBool::new(false),
        })
    }

    /// Keeps `stream` for reuse under `key`, unless the peer already closed it, sent data, or
    /// `idle_timeout` is zero. Returns how long the connection that was evicted to make room had
    /// been idle. Must be called inside a Tokio runtime.
    pub(crate) fn put(
        self: &Arc<Self>,
        key: u64,
        mut stream: Stream,
        idle_timeout: Option<Duration>,
    ) -> Option<Duration> {
        if self.capacity == 0 || idle_timeout == Some(Duration::ZERO) {
            return None;
        }
        let stripe = STRIPE.with(|stripe| *stripe);
        let id = stream.id();
        if !self.watch(&mut stream, stripe, id) {
            return None;
        }
        let released = Instant::now();
        let expires = idle_timeout.map(|timeout| released + timeout);
        let evicted = (self.idle.load(Ordering::Relaxed) >= self.capacity)
            .then(|| self.remove_oldest(stripe))
            .flatten();
        {
            let mut guard = self.stripes[stripe].0.lock();
            let sequence = guard.next_sequence;
            guard.next_sequence += 1;
            let idle = Idle {
                id,
                sequence,
                released,
                expires,
                runtime: runtime::Handle::current().id(),
                stream,
            };
            guard.by_key.entry(key).or_default().push_back(idle);
        }
        if evicted.is_none() {
            self.idle.fetch_add(1, Ordering::Relaxed);
        }
        if let Some(expires) = expires {
            self.expire_by(expires);
        }
        self.start_watcher();
        evicted.map(|idle| idle.released.elapsed())
    }

    /// The connection released longest ago in the stripe of the caller, or in the next stripe that
    /// holds one.
    fn remove_oldest(&self, first: usize) -> Option<Idle> {
        (0..self.stripes.len()).find_map(|offset| {
            self.stripes[(first + offset) % self.stripes.len()]
                .0
                .lock()
                .remove_oldest()
        })
    }

    #[cfg(all(test, feature = "any_tls"))]
    pub(crate) fn idle_count(&self) -> usize {
        self.idle.load(Ordering::Relaxed)
    }

    /// The most recently released connection under `key`.
    pub(crate) fn take(&self, key: u64) -> Option<Stream> {
        if self.idle.load(Ordering::Relaxed) == 0 {
            return None;
        }
        let first = STRIPE.with(|stripe| *stripe);
        for offset in 0..self.stripes.len() {
            let stripe = &self.stripes[(first + offset) % self.stripes.len()].0;
            let mut expired = Vec::new();
            let found = loop {
                let Some(idle) = stripe.lock().pop(key, |idle| idle.pop_back()) else {
                    break None;
                };
                self.idle.fetch_sub(1, Ordering::Relaxed);
                if idle
                    .expires
                    .is_some_and(|expires| expires <= Instant::now())
                {
                    expired.push(idle);
                } else if let Some(stream) = idle.into_current_runtime() {
                    break Some(stream);
                }
            };
            drop(expired);
            if found.is_some() {
                return found;
            }
        }
        None
    }

    /// Polls an idle connection once so that its peer closing, or sending data, wakes the pool
    /// task. Returns whether the connection is still idle and quiet.
    fn watch(&self, stream: &mut Stream, stripe: usize, id: i32) -> bool {
        let waker = Waker::from(Arc::new(IdleWake {
            wakeups: self.wakeups.clone(),
            stripe,
            id,
        }));
        let mut byte = [0; 1];
        let mut buffer = ReadBuf::new(&mut byte);
        let mut context = Context::from_waker(&waker);
        let polled = std::pin::Pin::new(stream).poll_read(&mut context, &mut buffer);
        if matches!(polled, Poll::Ready(Ok(()))) && !buffer.filled().is_empty() {
            self.unexpected_data.fetch_add(1, Ordering::Relaxed);
        }
        matches!(polled, Poll::Pending)
    }

    fn expire_by(&self, expires: Instant) {
        let mut deadline = self.wakeups.deadline.lock();
        if deadline.is_none_or(|current| expires < current) {
            *deadline = Some(expires);
            drop(deadline);
            self.wakeups.notify.notify_one();
        }
    }

    fn start_watcher(self: &Arc<Self>) {
        if !self.watcher_started.load(Ordering::Relaxed)
            && !self.watcher_started.swap(true, Ordering::Relaxed)
        {
            let wakeups = self.wakeups.clone();
            let pool = Arc::downgrade(self);
            pingora_runtime::current_handle().spawn(watcher(wakeups, pool));
        }
    }

    /// Drops the connections the kernel reported activity on and those past their timeout.
    fn maintain(&self) {
        let events = std::mem::take(&mut *self.wakeups.events.lock());
        let mut removed = Vec::new();
        for (stripe, id) in events {
            let mut guard = self.stripes[stripe].0.lock();
            guard.remove_where(
                |idle| idle.id == id && !self.watch(&mut idle.stream, stripe, id),
                &mut removed,
            );
        }
        let now = Instant::now();
        let mut earliest: Option<Instant> = None;
        for stripe in &self.stripes {
            stripe.0.lock().remove_where(
                |idle| match idle.expires {
                    Some(expires) if expires <= now => true,
                    Some(expires) => {
                        earliest = Some(earliest.map_or(expires, |current| current.min(expires)));
                        false
                    }
                    None => false,
                },
                &mut removed,
            );
        }
        *self.wakeups.deadline.lock() = earliest;
        self.idle.fetch_sub(removed.len(), Ordering::Relaxed);
    }
}

impl Drop for IdlePool {
    fn drop(&mut self) {
        self.wakeups.notify.notify_one();
    }
}

async fn watcher(wakeups: Arc<Wakeups>, pool: Weak<IdlePool>) {
    loop {
        let deadline = *wakeups.deadline.lock();
        match deadline {
            Some(deadline) => {
                tokio::select! {
                    () = wakeups.notify.notified() => {}
                    () = sleep_until(deadline) => {}
                }
            }
            None => wakeups.notify.notified().await,
        }
        let Some(pool) = pool.upgrade() else { return };
        pool.maintain();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connection_taken_by_another_runtime_is_registered_with_its_driver() {
        use tokio::io::AsyncReadExt;
        use std::io::Write;
        let build = || tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let (first, second) = (build(), build());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let pool = IdlePool::new(4, Arc::default());
        first.block_on(async {
            let client = tokio::net::TcpStream::connect(address).await.unwrap();
            let stream: Stream = Box::new(crate::protocols::l4::stream::Stream::from(client));
            pool.put(1, stream, None);
        });
        let (mut server, _) = listener.accept().unwrap();
        // The first runtime is never polled again: the byte reaches the second only if the socket moved to its driver.
        let received = second.block_on(async {
            let mut stream = pool.take(1).expect("pooled");
            server.write_all(b"x").unwrap();
            let mut byte = [0; 1];
            tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut byte)).await.is_ok()
        });
        assert!(received);
    }

    #[test]
    fn the_pool_has_a_stripe_for_every_cpu_and_at_least_eight() {
        let pool = IdlePool::new(1, Arc::default());
        let cpus = std::thread::available_parallelism().map_or(8, usize::from);
        assert_eq!(pool.stripes.len(), cpus.clamp(8, 256));
        assert!(STRIPE.with(|stripe| *stripe) < pool.stripes.len());
    }
}
