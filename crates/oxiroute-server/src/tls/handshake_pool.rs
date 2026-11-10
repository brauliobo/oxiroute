//! Threads that run the server side of TLS handshakes.
//!
//! A full handshake costs 0.6M to 1.7M instructions of key exchange and signature. Run inline it stalls every
//! connection ready on the same worker thread (data-path p99 grows 3x to 4x under a storm of new connections), so
//! the accepted socket is handed to this pool and the established stream comes back to its worker. The pool is
//! bounded: the threads, and the handshakes that were queued but not started yet (beyond that a connection is
//! dropped, as `max_connections` does). It is created on the first handshake, so a process without TLS traffic
//! pays nothing.
//!
//! `OXIROUTE_TLS_HANDSHAKE_THREADS` sets the thread count (`0` handshakes on the worker threads again; default one
//! thread per 8 CPUs, 1 to 4) and `OXIROUTE_TLS_HANDSHAKE_QUEUE` the queue bound (default 1024).

use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

use pingora::listeners::tls::HandshakeExecutor;
use tokio::{runtime::Runtime, task::JoinHandle};

const THREADS_ENV: &str = "OXIROUTE_TLS_HANDSHAKE_THREADS";
const QUEUE_ENV: &str = "OXIROUTE_TLS_HANDSHAKE_QUEUE";
const DEFAULT_QUEUE: usize = 1024;

pub(crate) struct HandshakePool {
    threads: usize,
    queue_limit: usize,
    runtime: OnceLock<Runtime>,
    queued: Arc<AtomicUsize>,
    started: Arc<AtomicU64>,
    rejected: AtomicU64,
}

/// What the process-wide handshake pool has done; zeros when it is disabled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HandshakePoolStats {
    /// Handshakes accepted and not started yet.
    pub queue_depth: usize,
    /// Handshakes started on the pool since the process began.
    pub started_total: u64,
    /// Handshakes refused because the queue was full, since the process began.
    pub rejected_total: u64,
}

/// A handshake that is queued and not started; dropping it (started or aborted before it ran) leaves the queue.
struct Queued(Arc<AtomicUsize>);

impl Drop for Queued {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

impl HandshakePool {
    fn new(threads: usize, queue_limit: usize) -> Self {
        Self {
            threads,
            queue_limit,
            runtime: OnceLock::new(),
            queued: Arc::new(AtomicUsize::new(0)),
            started: Arc::new(AtomicU64::new(0)),
            rejected: AtomicU64::new(0),
        }
    }

    fn runtime(&self) -> &Runtime {
        self.runtime.get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(self.threads)
                .thread_name("oxiroute-tls-handshake")
                .build()
                .expect("TLS handshake runtime starts")
        })
    }

    fn stats(&self) -> HandshakePoolStats {
        HandshakePoolStats {
            queue_depth: self.queued.load(Ordering::Relaxed),
            started_total: self.started.load(Ordering::Relaxed),
            rejected_total: self.rejected.load(Ordering::Relaxed),
        }
    }
}

impl HandshakeExecutor for HandshakePool {
    fn spawn(&self, job: Pin<Box<dyn Future<Output = ()> + Send>>) -> Option<JoinHandle<()>> {
        if self.queued.fetch_add(1, Ordering::Relaxed) >= self.queue_limit {
            self.queued.fetch_sub(1, Ordering::Relaxed);
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        let queued = Queued(Arc::clone(&self.queued));
        let started = Arc::clone(&self.started);
        Some(self.runtime().spawn(async move {
            drop(queued);
            started.fetch_add(1, Ordering::Relaxed);
            job.await;
        }))
    }
}

fn environment_usize(name: &str) -> Option<usize> {
    std::env::var(name).ok()?.parse().ok()
}

fn default_threads() -> usize {
    std::thread::available_parallelism().map_or(1, |cpus| cpus.get().div_ceil(8).clamp(1, 4))
}

/// The process-wide pool, `None` when `OXIROUTE_TLS_HANDSHAKE_THREADS=0` keeps handshakes on the worker threads.
pub(crate) fn pool() -> Option<&'static Arc<HandshakePool>> {
    static POOL: OnceLock<Option<Arc<HandshakePool>>> = OnceLock::new();
    POOL.get_or_init(|| {
        let threads = environment_usize(THREADS_ENV).unwrap_or_else(default_threads);
        let queue = environment_usize(QUEUE_ENV).unwrap_or(DEFAULT_QUEUE);
        (threads > 0).then(|| Arc::new(HandshakePool::new(threads, queue)))
    })
    .as_ref()
}

/// The counters of the process-wide pool.
#[must_use]
pub fn handshake_pool_stats() -> HandshakePoolStats {
    pool().map_or_else(HandshakePoolStats::default, |pool| pool.stats())
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, time::Duration};

    use super::*;

    fn job(work: impl FnOnce() + Send + 'static) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move { work() })
    }

    #[test]
    fn a_full_queue_refuses_handshakes_and_counts_them() {
        let pool = HandshakePool::new(1, 2);
        let (release, held) = mpsc::channel::<()>();
        let (started, running) = mpsc::channel::<()>();
        // The only thread blocks in the first job, so the next two stay queued.
        let first = pool.spawn(job(move || {
            started.send(()).unwrap();
            held.recv().unwrap();
        }));
        running.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(pool.stats().queue_depth, 0);
        let queued: Vec<_> = (0..2).map(|_| pool.spawn(job(|| ()))).collect();
        assert!(queued.iter().all(Option::is_some));
        assert_eq!(pool.stats().queue_depth, 2);
        assert!(pool.spawn(job(|| ())).is_none());
        assert_eq!(pool.stats().rejected_total, 1);
        release.send(()).unwrap();
        pool.runtime().block_on(async {
            first.unwrap().await.unwrap();
            for handle in queued.into_iter().flatten() {
                handle.await.unwrap();
            }
        });
        assert_eq!(pool.stats().queue_depth, 0);
    }

    #[test]
    fn an_aborted_handshake_leaves_the_queue() {
        let pool = HandshakePool::new(1, 4);
        let (release, held) = mpsc::channel::<()>();
        let (started, running) = mpsc::channel::<()>();
        pool.spawn(job(move || {
            started.send(()).unwrap();
            held.recv().unwrap();
        }));
        running.recv_timeout(Duration::from_secs(10)).unwrap();
        let waiting = pool.spawn(job(|| ())).unwrap();
        assert_eq!(pool.stats().queue_depth, 1);
        waiting.abort();
        release.send(()).unwrap();
        assert!(pool.runtime().block_on(waiting).unwrap_err().is_cancelled());
        assert_eq!(pool.stats().queue_depth, 0);
    }
}
