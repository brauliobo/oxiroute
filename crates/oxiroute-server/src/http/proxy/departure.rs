//! A client that leaves while its request waits for an origin takes the request with it. Before the first byte goes
//! to an origin (the retry delay, the pool's queue for a free slot, the connection to the origin) nothing of the
//! request is out of reach, so the wait is dropped as soon as the client's connection closes: its place in the queue
//! and its slot are free again and the origin never hears of the request.

use std::{future::Future, pin::pin};

use futures_util::FutureExt as _;
use pingora::{Error, ErrorType, protocols::Stream};

/// `wait`, unless the client leaves first, in which case `gone` says how. The watch on the client is only built when
/// the wait does not finish on its first poll, so a request that finds its server at once pays nothing for it.
pub(in crate::http::proxy) async fn unless_gone<T, G>(
    wait: impl Future<Output = Result<T, Box<Error>>>,
    gone: impl FnOnce() -> G,
) -> Result<T, Box<Error>>
where
    G: Future<Output = Box<Error>>,
{
    let mut wait = pin!(wait);
    if let Some(done) = wait.as_mut().now_or_never() {
        return done;
    }
    tokio::select! {
        biased;
        error = gone() => Err(error),
        done = wait => done,
    }
}

/// Resolves when the client closed its end of an HTTP/1 connection. Nothing is read, so it can be awaited while the
/// body of the request is still to come.
pub(in crate::http::proxy) async fn client_left(stream: &mut Stream) -> Box<Error> {
    let context = "downstream gone while its request waited";
    match stream.peer_closed().await {
        Ok(()) => Error::explain(ErrorType::ConnectionClosed, context),
        Err(cause) => Error::because(ErrorType::ConnectionClosed, context, cause),
    }
    .into_down()
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, future::pending};

    use super::*;

    #[allow(clippy::unnecessary_box_returns)]
    fn gone() -> Box<Error> {
        Error::explain(ErrorType::ConnectionClosed, "test").into_down()
    }

    #[tokio::test]
    async fn a_wait_that_finishes_at_once_builds_no_watch() {
        let built = Cell::new(false);
        let done = unless_gone(async { Ok(7) }, || {
            built.set(true);
            async { gone() }
        })
        .await;
        assert_eq!(done.expect("the wait finished"), 7);
        assert!(!built.get());
    }

    #[tokio::test]
    async fn a_client_that_leaves_ends_a_wait_that_would_go_on() {
        let waiting = pending::<Result<(), Box<Error>>>();
        let error = unless_gone(waiting, || async { gone() })
            .await
            .expect_err("the client left");
        assert_eq!(error.etype(), &ErrorType::ConnectionClosed);
    }

    #[tokio::test]
    async fn a_wait_that_finishes_later_is_not_cut_short() {
        let later = async {
            tokio::task::yield_now().await;
            Ok(1)
        };
        let done = unless_gone(later, pending::<Box<Error>>).await;
        assert_eq!(done.expect("the wait finished"), 1);
    }
}
