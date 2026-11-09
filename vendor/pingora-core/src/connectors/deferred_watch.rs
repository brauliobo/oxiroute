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

//! Idle watchers that start only for connections that stay idle.
//!
//! A released connection used to get a task of its own at once, which watches the socket for a close or
//! stray data and for the idle timeout. A connection of a busy proxy is picked up again within
//! microseconds, so that task was spawned, woken to exit and dropped for nothing: a few thousand
//! instructions per request. Here the watch is queued instead and one sweeper task starts the real watcher
//! of the connections still unused after one to two sweep periods. A connection reused earlier is
//! never watched; one checked out anyway is validated by `test_reusable_stream`.

use super::*;
use pingora_pool::ConnectionPool;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::{oneshot, Notify};

/// A connection idles unwatched for at least one and at most two of these.
pub(super) const SWEEP_PERIOD: Duration = Duration::from_millis(200);
const SHARDS: usize = 16;

type Pool = Arc<ConnectionPool<Arc<Mutex<Stream>>>>;

struct DeferredWatch {
    pool: Pool,
    meta: ConnectionMeta,
    stream: std::sync::Weak<Mutex<Stream>>,
    idle_timeout: Option<Duration>,
    notify_evicted: Arc<Notify>,
    watch_use: oneshot::Receiver<bool>,
    tick: u64,
}

impl DeferredWatch {
    /// Starts the watcher of a connection that is still idle; the others need none.
    fn start(mut self) {
        if !matches!(
            self.watch_use.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ) {
            return; // picked up, or evicted from the pool and closed
        }
        let Some(stream) = self.stream.upgrade() else {
            return;
        };
        // Checked out by now, or about to be: its new owner locks and takes it.
        let Ok(locked_stream) = stream.try_lock_owned() else {
            return;
        };
        spawn_idle_poll(
            self.pool,
            locked_stream,
            self.meta,
            self.idle_timeout,
            self.notify_evicted,
            self.watch_use,
        );
    }
}

pub(super) fn spawn_idle_poll(
    pool: Pool,
    locked_stream: tokio::sync::OwnedMutexGuard<Stream>,
    meta: ConnectionMeta,
    idle_timeout: Option<Duration>,
    notify_evicted: Arc<Notify>,
    watch_use: oneshot::Receiver<bool>,
) {
    pingora_runtime::current_handle().spawn(async move {
        pool.idle_poll(
            locked_stream,
            &meta,
            idle_timeout,
            notify_evicted,
            watch_use,
        )
        .await;
    });
}

/// The watches of one connector, with the sweeper that starts them.
pub(super) struct DeferredWatches {
    shards: [parking_lot::Mutex<Vec<DeferredWatch>>; SHARDS],
    tick: AtomicU64,
    sweeping: AtomicBool,
    queued: AtomicUsize,
}

thread_local! {
    static SHARD: usize = {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        NEXT.fetch_add(1, Ordering::Relaxed) % SHARDS
    };
}

/// Whether a connection with this idle timeout is worth deferring: a timeout the sweep period could
/// overshoot keeps its watcher from the start.
pub(super) fn defers(idle_timeout: Option<Duration>) -> bool {
    idle_timeout.is_none_or(|timeout| timeout >= SWEEP_PERIOD * 4)
}

impl DeferredWatches {
    pub(super) fn new() -> Self {
        Self {
            shards: std::array::from_fn(|_| parking_lot::Mutex::new(Vec::new())),
            tick: AtomicU64::new(0),
            sweeping: AtomicBool::new(false),
            queued: AtomicUsize::new(0),
        }
    }

    pub(super) fn defer(
        self: &Arc<Self>,
        pool: Pool,
        meta: ConnectionMeta,
        stream: std::sync::Weak<Mutex<Stream>>,
        idle_timeout: Option<Duration>,
        notify_evicted: Arc<Notify>,
        watch_use: oneshot::Receiver<bool>,
    ) {
        let watch = DeferredWatch {
            pool,
            meta,
            stream,
            idle_timeout,
            notify_evicted,
            watch_use,
            tick: self.tick.load(Ordering::Relaxed),
        };
        self.queued.fetch_add(1, Ordering::Relaxed);
        SHARD.with(|shard| self.shards[*shard].lock().push(watch));
        if !self.sweeping.swap(true, Ordering::AcqRel) {
            pingora_runtime::current_handle().spawn(Arc::clone(self).sweep());
        }
    }

    async fn sweep(self: Arc<Self>) {
        loop {
            tokio::time::sleep(SWEEP_PERIOD).await;
            let tick = self.tick.fetch_add(1, Ordering::Relaxed) + 1;
            for shard in &self.shards {
                let due = {
                    let mut queued = shard.lock();
                    let (due, young): (Vec<_>, Vec<_>) =
                        queued.drain(..).partition(|watch| watch.tick < tick);
                    *queued = young;
                    due
                };
                self.queued.fetch_sub(due.len(), Ordering::Relaxed);
                for watch in due {
                    watch.start();
                }
            }
            if self.queued.load(Ordering::Acquire) == 0 {
                self.sweeping.store(false, Ordering::Release);
                // A watch queued between the check and the store saw the sweeper running.
                if self.queued.load(Ordering::Acquire) == 0
                    || self.sweeping.swap(true, Ordering::AcqRel)
                {
                    return;
                }
            }
        }
    }
}
