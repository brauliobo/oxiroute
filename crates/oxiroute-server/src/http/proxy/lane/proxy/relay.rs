use std::{io::Write as _, pin::pin, time::Instant};

use bytes::Bytes;
use pingora::{
    Error, ErrorType,
    connectors::http::v1::Connector,
    protocols::{Stream, http::v1::client::HttpSession},
    upstreams::peer::Peer as _,
};

use super::{
    DIRECT_BODY, Framing, Opened, Progress,
    wire::{flush, read_piece, write},
};
use crate::http::proxy::{
    HttpRequestContext,
    attempt::AttemptTimeouts,
    lane::{
        buffers,
        timing::{Expired, Wait, within},
        upload::Upload,
    },
};

impl super::super::LaneFront {
    /// Writes the response head and relays the body, then gives the origin connection back to the pool.
    pub(super) async fn relay(
        &self,
        stream: &mut Stream,
        ctx: &mut HttpRequestContext,
        upstream: &Connector,
        opened: Opened,
        progress: &mut Progress,
    ) -> Result<(), Box<Error>> {
        let Opened {
            mut session,
            release,
            head,
            framing,
            buffered,
            mut upload,
            timeouts,
            ..
        } = opened;
        let deadline = tokio::time::Instant::from_std(ctx.deadline);
        let body = async {
            // `frame` carries the head and then each piece of the body with its chunk framing. `progress.sent` counts
            // the body, as a session does.
            let mut frame = head;
            if !buffered {
                write(stream, &frame).await?;
                flush(stream).await?;
                frame.clear();
            }
            let mut first = true;
            while let Some(data) = self
                .next_piece(
                    stream,
                    ctx,
                    &mut session,
                    (&mut upload, progress),
                    &timeouts,
                )
                .await?
            {
                progress.sent += data.len();
                // The first piece follows the clock reading of the head.
                let now = if std::mem::take(&mut first) {
                    ctx.now
                } else {
                    Instant::now()
                };
                ctx.observe_bytes(progress.received, progress.sent, now);
                let chunked = framing == Framing::Chunked;
                if chunked {
                    write!(frame, "{:x}\r\n", data.len()).expect("a buffer grows");
                }
                if data.len() < DIRECT_BODY {
                    frame.extend_from_slice(&data);
                    if chunked {
                        frame.extend_from_slice(b"\r\n");
                    }
                    write(stream, &frame).await?;
                } else {
                    // The staged head leaves with the piece in one vectored write, the piece is not copied.
                    write(stream, &frame).await?;
                    write(stream, &data).await?;
                    if chunked {
                        write(stream, b"\r\n").await?;
                    }
                }
                flush(stream).await?;
                frame.clear();
            }
            if !frame.is_empty() {
                write(stream, &frame).await?;
                frame.clear();
            }
            if framing == Framing::Chunked {
                write(stream, b"0\r\n\r\n").await?;
            }
            buffers::give_write(frame);
            flush(stream).await
        };
        within(Wait::Total, deadline, pin!(body))
            .await
            .map_err(|Expired| Error::new_up(ErrorType::ReadTimedout))??;
        // An origin that did not get the whole body keeps what it has not read: its connection is not used again.
        if upload.is_complete() {
            upstream
                .release_boxed_http_session(session, &release, release.idle_timeout())
                .await;
        }
        Ok(())
    }

    /// The next piece of the origin's response body. What is left of the request body goes to the origin meanwhile,
    /// as it arrives.
    async fn next_piece(
        &self,
        stream: &mut Stream,
        ctx: &mut HttpRequestContext,
        session: &mut HttpSession,
        (upload, progress): (&mut Upload, &mut Progress),
        timeouts: &AttemptTimeouts,
    ) -> Result<Option<Bytes>, Box<Error>> {
        while upload.pumping() {
            tokio::select! {
                piece = read_piece(session, timeouts) => return piece,
                ready = upload.readable(stream, self.timeouts.request) => {
                    ready?;
                    match upload.forward(stream, session).await {
                        Ok(count) => {
                            progress.received += count;
                            ctx.observe_bytes(progress.received, progress.sent, Instant::now());
                        }
                        // The origin took no more of the body; its response goes on.
                        Err(error) if error.esource() == &pingora::ErrorSource::Upstream => upload.refuse(),
                        Err(error) => return Err(error),
                    }
                }
            }
        }
        read_piece(session, timeouts).await
    }
}
