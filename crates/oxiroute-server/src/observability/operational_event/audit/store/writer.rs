#[allow(clippy::wildcard_imports)]
use super::*;

/// Commits pending batched records once the oldest has waited `FLUSH_DELAY`, and everything left at shutdown.
///
/// The thread sleeps on the condition variable while nothing is pending, so an idle store costs no wakeups.
pub(super) fn run(shared: &Shared) {
    let mut state = lock(&shared.state);
    loop {
        if state.pending.is_empty() {
            if state.shutdown {
                return;
            }
            state = shared
                .wake
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
            continue;
        }
        if !state.shutdown
            && let Some(due) = state.pending_since.map(|since| since + FLUSH_DELAY)
            && let Some(wait) = due.checked_duration_since(Instant::now())
        {
            state = shared
                .wake
                .wait_timeout(state, wait)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
            continue;
        }
        drop(state);
        if let Err(error) = shared.flush() {
            log::warn!(target: "oxiroute::audit", "durable audit write failed: {}", error.code());
        }
        state = lock(&shared.state);
    }
}
