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

use log::debug;
use once_cell::sync::OnceCell;
use rand::Rng;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::runtime::{Builder, Handle};
use tokio::sync::oneshot::{channel, Sender};
use tokio::task::{JoinError, JoinHandle};

// NOTE: use dedicated current-thread runtimes until pingora-runtime can preserve
// the lazy-after-daemonize initialization behavior below.
/// Lazily initialized runtime pools for offloading work from request runtimes.
///
/// The runtime is split into `shards`, with `thread_per_shard` single-threaded
/// Tokio runtimes per shard. [`Self::get_runtime`] picks a shard from the caller
/// supplied hash and then picks one runtime within that shard at random.
pub(crate) struct OffloadRuntime {
    thread_name: &'static str,
    shards: usize,
    thread_per_shard: usize,
    // Lazily init the runtimes so that they are created after pingora
    // daemonize itself. Otherwise the runtime threads are lost.
    pools: OnceCell<Box<[(Handle, Sender<()>)]>>,
}

impl OffloadRuntime {
    /// Create an offload runtime pool whose threads use `thread_name`.
    ///
    /// The actual threads are started lazily by [`Self::get_runtime`] so that
    /// services which daemonize do not lose the runtime threads.
    ///
    /// # Panics
    ///
    /// Panics when either `shards` or `thread_per_shard` is zero.
    #[track_caller]
    pub fn new(thread_name: &'static str, shards: usize, thread_per_shard: usize) -> Self {
        assert!(shards != 0, "shards must be greater than zero");
        assert!(
            thread_per_shard != 0,
            "thread_per_shard must be greater than zero"
        );
        OffloadRuntime {
            thread_name,
            shards,
            thread_per_shard,
            pools: OnceCell::new(),
        }
    }

    /// Build every runtime thread in this pool.
    fn init_pools(&self) -> Box<[(Handle, Sender<()>)]> {
        let threads = self.shards * self.thread_per_shard;
        let mut pools = Vec::with_capacity(threads);
        for shard in 0..self.shards {
            for thread in 0..self.thread_per_shard {
                // We use single thread runtimes to reduce the scheduling overhead of multithread
                // tokio runtime, which can be 50% of the on CPU time of the runtimes
                let rt = Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("failed to build offload runtime");
                let handler = rt.handle().clone();
                let (tx, rx) = channel::<()>();
                let thread_name = format!("{} {shard}.{thread}", self.thread_name);
                std::thread::Builder::new()
                    .name(thread_name.clone())
                    .spawn(move || {
                        debug!("{thread_name} started");
                        // the thread that calls block_on() will drive the runtime
                        // rx will return when tx is dropped so this runtime and thread will exit
                        rt.block_on(rx)
                    })
                    .expect("failed to spawn offload runtime thread");
                pools.push((handler, tx));
            }
        }

        pools.into_boxed_slice()
    }

    /// Run `future` on the runtime for `hash`.
    ///
    /// `hash` selects the shard. A runtime within that shard is chosen randomly
    /// to spread work across `thread_per_shard` runtimes. The task ends when the returned
    /// [`OffloadTask`] is dropped.
    pub fn spawn<F>(&self, hash: u64, future: F) -> OffloadTask<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        OffloadTask(self.get_runtime(hash).spawn(future))
    }

    fn get_runtime(&self, hash: u64) -> &Handle {
        let mut rng = rand::thread_rng();

        // choose a shard based on hash and a random thread with in that shard
        // e.g. say thread_per_shard=2, shard 1 thread 1 is 1 * 2 + 1 = 3
        // [[th0, th1], [th2, th3], ...]
        let shard = hash as usize % self.shards;
        let thread_in_shard = rng.gen_range(0..self.thread_per_shard);
        let pools = self.pools.get_or_init(|| self.init_pools());
        &pools[shard * self.thread_per_shard + thread_in_shard].0
    }
}

/// A task running on an offload runtime. Dropping it aborts the task, so a caller that stops waiting for it (a
/// handshake timeout, a cancelled connection) does not leave it holding the socket and the connection admission
/// it owns.
pub(crate) struct OffloadTask<T>(JoinHandle<T>);

impl<T> Future for OffloadTask<T> {
    type Output = Result<T, JoinError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx)
    }
}

impl<T> Drop for OffloadTask<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{channel, Sender};
    use std::time::Duration;

    struct Held(Sender<()>);

    impl Drop for Held {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }

    #[test]
    fn a_dropped_task_releases_what_it_holds() {
        let offload = OffloadRuntime::new("test offload", 1, 1);
        let (released, released_rx) = channel();
        let held = Held(released);
        let task = offload.spawn(0, async move {
            let _held = held;
            std::future::pending::<()>().await
        });
        drop(task);
        released_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the aborted task released its resource");
    }

    #[test]
    fn a_finished_task_returns_its_output() {
        let offload = OffloadRuntime::new("test offload", 1, 1);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let output = runtime.block_on(offload.spawn(0, async { 7 })).unwrap();
        assert_eq!(output, 7);
    }
}
