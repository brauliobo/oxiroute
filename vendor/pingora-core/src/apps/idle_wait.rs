//! The wait between two requests of a keep-alive HTTP/1 connection, spent without a session or a request future.

use std::time::Duration;

use pingora_timeout::timeout;

use crate::protocols::Stream;
use crate::server::ShutdownWatch;

/// Waits until `stream` has bytes to read. Returns `false` when the connection ends instead: the server is stopping,
/// or `idle` passed without a byte; the server stopping counts only when `shutdown` is given. A stream that cannot tell (TLS) is ready at once, and the request read that follows
/// does the waiting as before. Bytes already buffered or readable win over the shutdown signal, so a request that has
/// arrived is still answered.
pub(super) async fn wait_for_next_request(
    stream: &mut Stream,
    idle: Option<Duration>,
    shutdown: Option<&ShutdownWatch>,
) -> bool {
    let readable = std::future::poll_fn(|cx| stream.poll_read_ready(cx));
    let stopping = async {
        match shutdown {
            Some(shutdown) => {
                let _ = shutdown.clone().wait_for(|stopping| *stopping).await;
            }
            None => std::future::pending().await,
        }
    };
    let wait = async {
        tokio::select! {
            biased;
            _ = readable => true,
            () = stopping => false,
        }
    };
    match idle {
        Some(idle) => timeout(idle, wait).await.unwrap_or(false),
        None => wait.await,
    }
}
