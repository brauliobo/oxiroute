//! Clock reads of the request path.
//!
//! A request reads the clock where time can have passed (arrival, the upstream's answer, the end) and reuses
//! that reading for every step it runs without waiting. `promptly` keeps a future that is ready at once from
//! creating a timer, which would read the clock for a deadline nobody needs.

use std::{
    cell::Cell,
    future::Future,
    pin::pin,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

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

/// How long a thread trusts the wall clock reading it derived its offsets from.
const REANCHOR: Duration = Duration::from_secs(1);

thread_local! {
    static ANCHOR: Cell<Option<(Instant, u64)>> = const { Cell::new(None) };
}

fn system_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

/// Unix time in milliseconds at the monotonic reading `now`, from a reading of the wall clock this thread took
/// at most a second before: a record that already holds a reading for its own timing needs no second clock read.
pub(crate) fn wall_ms(now: Instant) -> u64 {
    ANCHOR.with(|anchor| {
        if let Some((at, unix_ms)) = anchor.get()
            && let Some(elapsed) = now.checked_duration_since(at)
            && elapsed < REANCHOR
        {
            return unix_ms + u64::try_from(elapsed.as_millis()).unwrap_or(0);
        }
        let unix_ms = system_unix_ms();
        anchor.set(Some((now, unix_ms)));
        unix_ms
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_time_from_a_reading_follows_the_system_clock() {
        let first = wall_ms(Instant::now());
        std::thread::sleep(Duration::from_millis(30));
        let second = wall_ms(Instant::now());
        assert!((30..250).contains(&(second - first)), "{first} {second}");
        assert!(second.abs_diff(system_unix_ms()) <= 5);
        let earlier = Instant::now();
        std::thread::sleep(Duration::from_millis(5));
        let _ = wall_ms(Instant::now());
        assert!(
            wall_ms(earlier).abs_diff(system_unix_ms()) <= 10,
            "an older reading"
        );
    }

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
