//! Threads that run the server side of TLS handshakes.
//!
//! A full handshake costs 0.6M to 1.7M instructions of key exchange and signature. Run inline it stalls every
//! connection ready on the same worker thread (data-path p99 grows 3x to 4x under a storm of new connections), so
//! Pingora's TLS offload hands the accepted socket to dedicated single-threaded runtimes and returns the
//! established stream to its worker. Connections stay counted by the admission guard and bounded by the handshake
//! timeout while they wait, so a storm queues at most `max_connections` handshakes. The threads start with the
//! first handshake.
//!
//! `OXIROUTE_TLS_HANDSHAKE_THREADS` sets the thread count (`0` handshakes on the worker threads again; default one
//! thread per 8 CPUs, 1 to 4).

const THREADS_ENV: &str = "OXIROUTE_TLS_HANDSHAKE_THREADS";

fn default_threads() -> usize {
    std::thread::available_parallelism().map_or(1, |cpus| cpus.get().div_ceil(8).clamp(1, 4))
}

/// Handshake threads for each TLS listener; `None` keeps handshakes on the worker threads.
pub(crate) fn handshake_threads() -> Option<usize> {
    let configured = std::env::var(THREADS_ENV)
        .ok()
        .and_then(|value| value.parse().ok());
    Some(configured.unwrap_or_else(default_threads)).filter(|&threads| threads > 0)
}
