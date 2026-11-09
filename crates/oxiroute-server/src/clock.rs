//! Clock reads of the request path.
//!
//! A request reads the clock where time can have passed (arrival, the upstream's answer, the end) and reuses
//! that reading for every step it runs without waiting. `promptly` keeps a future that is ready at once from
//! creating a timer, which would read the clock for a deadline nobody needs.

use std::{future::Future, pin::pin, time::Duration};

use futures_util::FutureExt as _;
use tokio::time::error::Elapsed;

/// The output of `future` and whether it had to wait. A future that is ready on its first poll needs no timer;
/// otherwise it gets the remaining part of `limit`.
pub(crate) async fn promptly<F: Future>(
    limit: Duration,
    future: F,
) -> Result<(F::Output, bool), Elapsed> {
    let mut future = pin!(future);
    if let Some(output) = future.as_mut().now_or_never() {
        return Ok((output, false));
    }
    tokio::time::timeout(limit, future)
        .await
        .map(|output| (output, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ready_future_needs_no_timer() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime without a time driver");
        let ready = runtime.block_on(promptly(Duration::from_secs(1), async { 7 }));
        assert_eq!(ready.expect("ready"), (7, false));
    }

    #[test]
    fn a_pending_future_waits_and_is_bounded() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let sleeps = promptly(Duration::from_secs(5), async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                9
            })
            .await;
            assert_eq!(sleeps.expect("completes in time"), (9, true));
            let never = promptly(Duration::from_secs(1), std::future::pending::<()>()).await;
            assert!(never.is_err());
        });
    }
}
