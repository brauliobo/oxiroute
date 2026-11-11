//! The body of a request on the lane: read from the client with the framing reader of the session, piece by piece, and
//! written to the origin as it arrives. Nothing is buffered beyond the piece in hand.

use std::time::Duration;

use bytes::Bytes;
use pingora::{
    Error, ErrorType,
    protocols::{Stream, http::v1::body::BodyReader},
};

use super::head::Body;

pub(super) struct Upload {
    reader: BodyReader,
    received: usize,
    limit: Option<u64>,
    read_timeout: Option<Duration>,
}

impl Upload {
    /// `preread` is what followed the head in the bytes already read. The caller has refused a content length that
    /// `preread` exceeds, so no byte of a pipelined request is in it.
    pub(super) fn new(
        body: Body,
        preread: &[u8],
        limit: Option<u64>,
        read_timeout: Option<Duration>,
    ) -> Self {
        let mut reader = BodyReader::new(false);
        match body {
            Body::Length(length) => reader.init_content_length_preread(
                usize::try_from(length).expect("a scanned length"),
                Bytes::copy_from_slice(preread),
            ),
            Body::Chunked => reader.init_chunked(preread),
            Body::None => unreachable!("only requests with a body have an upload"),
        }
        Self {
            reader,
            received: 0,
            limit,
            read_timeout,
        }
    }

    /// The next piece of the body; `None` at its end. A body over the route's limit is the classic `413`. The reader
    /// reports the framing of a chunked body (a chunk size, the last chunk) as empty pieces, which carry nothing.
    pub(super) async fn next(&mut self, stream: &mut Stream) -> Result<Option<Bytes>, Box<Error>> {
        loop {
            let read = self.reader.read_body_bytes(stream);
            let piece = match self.read_timeout {
                Some(limit) => tokio::time::timeout(limit, read)
                    .await
                    .map_err(|_| Error::new_down(ErrorType::ReadTimedout))??,
                None => read.await?,
            };
            let Some(piece) = piece else {
                return Ok(None);
            };
            if piece.is_empty() {
                continue;
            }
            self.received += piece.len();
            if self
                .limit
                .is_some_and(|limit| u64::try_from(self.received).is_ok_and(|bytes| bytes > limit))
            {
                return Err(Error::new_down(ErrorType::HTTPStatus(413)));
            }
            return Ok(Some(piece));
        }
    }

    pub(super) const fn received(&self) -> usize {
        self.received
    }

    /// Whether the client sent bytes past the end of the body: the start of a pipelined request, which the classic
    /// path does not serve either (it ends the connection after the response).
    pub(super) fn overread(&self) -> bool {
        self.reader.has_bytes_overread()
    }
}
