//! The body on the stream: flow control, the wait for the client's window bounded by the write timeout, and the
//! client's departure noticed while the lane waits for the origin.

use std::{
    future::{Future, poll_fn},
    pin::Pin,
    task::Poll,
    time::Duration,
};

use bytes::Bytes;
use pingora::{Error, ErrorType, protocols::http::v2::server::H2Stream};
use tokio::time::{Sleep, sleep};

/// The work did not finish within its time.
struct Expired;

/// Runs `work` for at most `limit`. The timer is armed the first time `work` cannot go on, so work that does not
/// wait (a window that has room) never touches the timer wheel.
async fn within_for<F: Future>(limit: Option<Duration>, work: F) -> Result<F::Output, Expired> {
    let mut work = std::pin::pin!(work);
    let mut timer: Option<Pin<Box<Sleep>>> = None;
    poll_fn(|cx| {
        if let Poll::Ready(output) = work.as_mut().poll(cx) {
            return Poll::Ready(Ok(output));
        }
        let Some(limit) = limit else {
            return Poll::Pending;
        };
        let timer = timer.get_or_insert_with(|| Box::pin(sleep(limit)));
        timer.as_mut().poll(cx).map(|()| Err(Expired))
    })
    .await
}

async fn reserve_and_send(
    stream: &mut H2Stream,
    data: &mut Bytes,
    end: bool,
) -> Result<(), Box<Error>> {
    let want = data.len();
    let granted = poll_fn(|cx| stream.poll_capacity(cx, want))
        .await
        .map_err(|error| {
            Error::because(ErrorType::H2Error, "while waiting for capacity", error).into_down()
        })?;
    let chunk = data.split_to(granted.min(want));
    send_data(stream, chunk, data.is_empty() && end)
}

fn send_data(stream: &mut H2Stream, data: Bytes, end: bool) -> Result<(), Box<Error>> {
    stream.send_data(data, end).map_err(|error| {
        Error::because(
            ErrorType::WriteError,
            "while writing h2 response body",
            error,
        )
        .into_down()
    })
}

/// Sends `data` as the response body, waiting for window as often as needed; every wait is bounded by
/// `write_timeout`. `end` ends the stream with the last byte.
pub(super) async fn send_body(
    stream: &mut H2Stream,
    mut data: Bytes,
    end: bool,
    write_timeout: Option<Duration>,
) -> Result<(), Box<Error>> {
    // Capacity cannot be polled for nothing, so an empty piece goes out directly.
    if data.is_empty() {
        return send_data(stream, data, end);
    }
    while !data.is_empty() {
        let step = reserve_and_send(stream, &mut data, end);
        within_for(write_timeout, step).await.map_err(|Expired| {
            Error::explain(
                ErrorType::WriteTimedout,
                format!("while writing h2 response body, timeout: {write_timeout:?}"),
            )
            .into_down()
        })??;
    }
    Ok(())
}

/// Ends a stream whose body went out without `END_STREAM`.
pub(super) fn finish(stream: &mut H2Stream) -> Result<(), Box<Error>> {
    send_data(stream, Bytes::new(), true)
}

/// Resolves when the client reset the stream or the connection closed: the error the classic session raises when
/// it waits for the downstream to go away.
pub(super) async fn client_left(stream: &mut H2Stream) -> Box<Error> {
    let error = match poll_fn(|cx| stream.poll_reset(cx)).await {
        Ok(reason) => Error::explain(
            ErrorType::H2Error,
            format!("Client closed H2, reason: {reason}"),
        ),
        Err(error) => Error::because(ErrorType::H2Error, "downstream error while idling", error),
    };
    error.into_down()
}
