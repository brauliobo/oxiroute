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

//! Pingora tokio runtime.
//!
//! Tokio runtime comes in two flavors: one current-thread runtime per worker thread (no work stealing) and a
//! multi-threaded one which provides work stealing. OxiRoute delta: `spawn_connection` hands a new connection
//! to the least loaded worker of a non-stealing runtime.

use once_cell::sync::{Lazy, OnceCell};
use rand::Rng;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;
use thread_local::ThreadLocal;
use tokio::runtime::{Builder, Handle};
use tokio::sync::oneshot::{channel, Sender};

pub enum Runtime {
    Steal(tokio::runtime::Runtime),
    NoSteal(NoStealRuntime),
}

impl Runtime {
    pub fn new_steal(threads: usize, name: &str) -> Self {
        Self::Steal(
            Builder::new_multi_thread()
                .enable_all()
                .worker_threads(threads)
                .thread_name(name)
                .build()
                .unwrap(),
        )
    }

    pub fn new_no_steal(threads: usize, name: &str) -> Self {
        Self::NoSteal(NoStealRuntime::new(threads, name))
    }

    pub fn get_handle(&self) -> &Handle {
        match self {
            Self::Steal(r) => r.handle(),
            Self::NoSteal(r) => r.get_runtime(),
        }
    }

    pub fn shutdown_timeout(self, timeout: Duration) {
        match self {
            Self::Steal(r) => r.shutdown_timeout(timeout),
            Self::NoSteal(r) => r.shutdown_timeout(timeout),
        }
    }
}

struct Pool {
    handle: Handle,
    load: AtomicUsize,
}

struct Local {
    pools: Pools,
    index: usize,
}

static CURRENT_HANDLE: Lazy<ThreadLocal<Local>> = Lazy::new(ThreadLocal::new);

pub fn current_handle() -> Handle {
    if let Some(local) = CURRENT_HANDLE.get() {
        let pools = local.pools.get().unwrap();
        let mut rng = rand::thread_rng();
        let index = rng.gen_range(0..pools.len());
        pools[index].handle.clone()
    } else {
        Handle::current()
    }
}

struct LoadGuard {
    pools: Pools,
    index: usize,
}

impl Drop for LoadGuard {
    fn drop(&mut self) {
        self.pools.get().unwrap()[self.index]
            .load
            .fetch_sub(1, Ordering::Relaxed);
    }
}

/// Spawns a connection task. On a non-stealing runtime the least loaded worker takes it (ties go to the
/// calling worker); on a work stealing runtime it is a plain `spawn`.
pub fn spawn_connection<F>(future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    let Some(local) = CURRENT_HANDLE.get() else {
        Handle::current().spawn(future);
        return;
    };
    let pools = local.pools.get().unwrap();
    let count = pools.len();
    let index = (0..count)
        .map(|step| (local.index + step) % count)
        .min_by_key(|index| pools[*index].load.load(Ordering::Relaxed))
        .unwrap();
    pools[index].load.fetch_add(1, Ordering::Relaxed);
    let guard = LoadGuard {
        pools: local.pools.clone(),
        index,
    };
    pools[index].handle.spawn(async move {
        let _guard = guard;
        future.await
    });
}

type Control = (Sender<Duration>, JoinHandle<()>);
type Pools = Arc<OnceCell<Box<[Pool]>>>;

pub struct NoStealRuntime {
    threads: usize,
    name: String,
    pools: Pools,
    controls: OnceCell<Vec<Control>>,
}

impl NoStealRuntime {
    pub fn new(threads: usize, name: &str) -> Self {
        assert!(threads != 0);
        NoStealRuntime {
            threads,
            name: name.to_string(),
            pools: Arc::new(OnceCell::new()),
            controls: OnceCell::new(),
        }
    }

    fn init_pools(&self) -> (Box<[Pool]>, Vec<Control>) {
        let mut pools = Vec::with_capacity(self.threads);
        let mut controls = Vec::with_capacity(self.threads);
        for index in 0..self.threads {
            let rt = Builder::new_current_thread().enable_all().build().unwrap();
            let handle = rt.handle().clone();
            let (tx, rx) = channel::<Duration>();
            let pools_ref = self.pools.clone();
            let join = std::thread::Builder::new()
                .name(self.name.clone())
                .spawn(move || {
                    CURRENT_HANDLE.get_or(|| Local {
                        pools: pools_ref,
                        index,
                    });
                    if let Ok(timeout) = rt.block_on(rx) {
                        rt.shutdown_timeout(timeout);
                    } // else Err(_): tx is dropped, just exit
                })
                .unwrap();
            pools.push(Pool {
                handle,
                load: AtomicUsize::new(0),
            });
            controls.push((tx, join));
        }

        (pools.into_boxed_slice(), controls)
    }

    pub fn get_runtime(&self) -> &Handle {
        let mut rng = rand::thread_rng();

        let index = rng.gen_range(0..self.threads);
        self.get_runtime_at(index)
    }

    pub fn threads(&self) -> usize {
        self.threads
    }

    fn get_pools(&self) -> &[Pool] {
        if let Some(p) = self.pools.get() {
            p
        } else {
            let (pools, controls) = self.init_pools();
            match self.pools.try_insert(pools) {
                Ok(p) => {
                    self.controls.set(controls).unwrap();
                    p
                }
                Err((p, _my_pools)) => p,
            }
        }
    }

    pub fn get_runtime_at(&self, index: usize) -> &Handle {
        &self.get_pools()[index].handle
    }

    pub fn shutdown_timeout(mut self, timeout: Duration) {
        if let Some(controls) = self.controls.take() {
            let (txs, joins): (Vec<Sender<_>>, Vec<JoinHandle<()>>) = controls.into_iter().unzip();
            for tx in txs {
                let _ = tx.send(timeout); // Err() when rx is dropped
            }
            for join in joins {
                let _ = join.join(); // ignore thread error
            }
        } // else, the controls and the runtimes are not even init yet, just return;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::thread::ThreadId;
    use tokio::sync::{mpsc, Notify};
    use tokio::time::{sleep, Duration};

    /// Spawns `count` connection tasks from a worker of `runtime`; each reports its thread and then waits to be
    /// released. Returns the number of tasks per worker thread.
    fn spread(runtime: &Runtime, count: usize) -> (HashMap<ThreadId, usize>, Arc<Notify>) {
        let release = Arc::new(Notify::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let release_all = release.clone();
        let handle = runtime.get_handle().clone();
        handle.block_on_spawned(async move {
            for _ in 0..count {
                let (tx, release) = (tx.clone(), release_all.clone());
                spawn_connection(async move {
                    tx.send(std::thread::current().id()).unwrap();
                    release.notified().await;
                });
            }
        });
        let mut per_thread = HashMap::new();
        for _ in 0..count {
            *per_thread
                .entry(rx.blocking_recv().unwrap())
                .or_default() += 1;
        }
        (per_thread, release)
    }

    trait BlockOnSpawned {
        fn block_on_spawned<F: Future<Output = ()> + Send + 'static>(&self, future: F);
    }

    impl BlockOnSpawned for Handle {
        fn block_on_spawned<F: Future<Output = ()> + Send + 'static>(&self, future: F) {
            let (done, wait) = std::sync::mpsc::channel();
            self.spawn(async move {
                future.await;
                done.send(()).unwrap();
            });
            wait.recv().unwrap();
        }
    }

    #[test]
    fn connections_spread_evenly_over_the_workers() {
        let runtime = Runtime::new_no_steal(4, "spread");
        let (per_thread, release) = spread(&runtime, 64);
        assert_eq!(per_thread.len(), 4);
        assert!(per_thread.values().all(|count| *count == 16), "{per_thread:?}");
        release.notify_waiters();
        runtime.shutdown_timeout(Duration::from_secs(1));
    }

    #[test]
    fn a_finished_connection_gives_its_slot_back() {
        let runtime = Runtime::new_no_steal(2, "slots");
        for _ in 0..2 {
            let (per_thread, release) = spread(&runtime, 10);
            assert_eq!(per_thread.values().copied().collect::<Vec<_>>(), [5, 5]);
            release.notify_waiters();
            std::thread::sleep(Duration::from_millis(100));
        }
        runtime.shutdown_timeout(Duration::from_secs(1));
    }

    #[test]
    fn an_idle_pool_keeps_connections_on_the_accepting_worker() {
        let runtime = Runtime::new_no_steal(4, "local");
        let (tx, rx) = std::sync::mpsc::channel();
        runtime.get_handle().spawn(async move {
            let caller = std::thread::current().id();
            for _ in 0..8 {
                let (done, finished) = tokio::sync::oneshot::channel();
                spawn_connection(async move {
                    done.send(std::thread::current().id()).unwrap();
                });
                assert_eq!(finished.await.unwrap(), caller);
                sleep(Duration::from_millis(1)).await;
            }
            tx.send(()).unwrap();
        });
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        runtime.shutdown_timeout(Duration::from_secs(1));
    }

    #[test]
    fn a_work_stealing_runtime_spawns_on_itself() {
        let runtime = Runtime::new_steal(2, "steal");
        let (tx, rx) = std::sync::mpsc::channel();
        runtime.get_handle().block_on_spawned(async move {
            spawn_connection(async move { tx.send(()).unwrap() });
        });
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }
}
