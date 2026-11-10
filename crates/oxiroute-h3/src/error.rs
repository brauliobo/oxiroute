//! Errors of the connection and of the request stream handles.
use crate::code::Code;

/// Why an operation on the connection or a request stream failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The QUIC connection is closed or lost.
    #[error("connection closed: {0}")]
    Connection(#[from] quinn::ConnectionError),
    /// The peer violated the protocol; this layer closed the connection with `code`.
    #[error("protocol violation ({code}): {reason}")]
    Protocol { code: Code, reason: &'static str },
    /// This layer reset the request stream with `code` (an oversized `HEADERS` frame, for example).
    #[error("request stream reset ({code}): {reason}")]
    Stream { code: Code, reason: &'static str },
    /// The peer reset the request stream.
    #[error("request stream reset by the peer with code {0}")]
    StreamReset(quinn::VarInt),
    /// The peer stopped reading the response.
    #[error("response stream stopped by the peer with code {0}")]
    StreamStopped(quinn::VarInt),
    /// The stream ended before a complete message arrived.
    #[error("request stream ended before the message was complete")]
    Incomplete,
    /// The operation does not fit the stream state (sending after `finish`, a second response, and so on).
    #[error("operation does not fit the stream state: {0}")]
    State(&'static str),
}

impl From<quinn::ReadError> for Error {
    fn from(error: quinn::ReadError) -> Self {
        match error {
            quinn::ReadError::Reset(code) => Error::StreamReset(code),
            quinn::ReadError::ConnectionLost(error) => Error::Connection(error),
            quinn::ReadError::ClosedStream | quinn::ReadError::IllegalOrderedRead => {
                Error::State("receive side already closed")
            }
            quinn::ReadError::ZeroRttRejected => Error::State("0-RTT rejected"),
        }
    }
}

impl From<quinn::WriteError> for Error {
    fn from(error: quinn::WriteError) -> Self {
        match error {
            quinn::WriteError::Stopped(code) => Error::StreamStopped(code),
            quinn::WriteError::ConnectionLost(error) => Error::Connection(error),
            quinn::WriteError::ClosedStream => Error::State("send side already closed"),
            quinn::WriteError::ZeroRttRejected => Error::State("0-RTT rejected"),
        }
    }
}

impl From<quinn::ClosedStream> for Error {
    fn from(_: quinn::ClosedStream) -> Self {
        Error::State("send side already closed")
    }
}

/// Why the request head could not be delivered.
#[derive(Debug, thiserror::Error)]
pub enum HeadError {
    /// The decoded field section is larger than the configured limit. The stream is intact: the application answers
    /// (normally 431) and finishes it.
    #[error("field section of at least {size} bytes exceeds the limit")]
    FieldSectionTooLarge { size: u64 },
    /// The request is malformed. The stream was reset with `H3_MESSAGE_ERROR` in both directions.
    #[error("malformed request: {0}")]
    Malformed(&'static str),
    /// The stream or the connection failed.
    #[error(transparent)]
    Error(#[from] Error),
}
