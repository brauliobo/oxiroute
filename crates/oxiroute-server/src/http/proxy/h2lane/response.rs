//! The response on the stream: the head as `HttpSession::write_response_header` of the classic HTTP/2 session
//! writes it, and the error answers.

use bytes::Bytes;
use http::{
    HeaderMap, HeaderName, StatusCode,
    header::{CONNECTION, DATE, TRANSFER_ENCODING, UPGRADE},
};
use pingora::{
    Error, ErrorType,
    http::ResponseHeader,
    protocols::http::{date::get_cached_date, v2::server::H2Stream},
};

use super::relay;

const KEEP_ALIVE: HeaderName = HeaderName::from_static("keep-alive");
const PROXY_CONNECTION: HeaderName = HeaderName::from_static("proxy-connection");

/// The response head of the origin's answer: made from the codec's scan (`fast`), with the automatic `Date` in place,
/// or the classic parser's `ResponseHeader`, which still needs it.
pub(super) enum Head {
    Fields(StatusCode, HeaderMap),
    Classic(ResponseHeader),
}

impl Head {
    pub(super) fn status(&self) -> StatusCode {
        match self {
            Self::Fields(status, _) => *status,
            Self::Classic(head) => head.status,
        }
    }

    pub(super) fn send(self, stream: &mut H2Stream, end: bool) -> Result<(), Box<Error>> {
        match self {
            Self::Fields(status, headers) => send_fields(stream, status, headers, end),
            Self::Classic(head) => send_head(stream, head, end),
        }
    }
}

fn send_fields(
    stream: &mut H2Stream,
    status: StatusCode,
    headers: HeaderMap,
    end: bool,
) -> Result<(), Box<Error>> {
    stream.send_response(status, headers, end).map_err(|error| {
        Error::because(
            ErrorType::WriteError,
            "while writing h2 response to downstream",
            error,
        )
        .into_down()
    })
}

/// Sends the response head: the automatic `Date` replaces any other, the fields that cannot be in HTTP/2 go.
/// `end` ends the stream with the head.
pub(super) fn send_head(
    stream: &mut H2Stream,
    mut head: ResponseHeader,
    end: bool,
) -> Result<(), Box<Error>> {
    head.insert_header(DATE, get_cached_date())
        .map_err(Error::into_down)?;
    for name in [
        &TRANSFER_ENCODING,
        &CONNECTION,
        &UPGRADE,
        &KEEP_ALIVE,
        &PROXY_CONNECTION,
    ] {
        head.remove_header(name);
    }
    let parts: http::response::Parts = head.into();
    send_fields(stream, parts.status, parts.headers, end)
}

/// A response the lane makes itself, as `write_error_response` writes it: the head without `END_STREAM`, then
/// the body or an empty `DATA` that ends the stream.
pub(super) async fn send_local(
    stream: &mut H2Stream,
    head: ResponseHeader,
    body: Bytes,
    write_timeout: Option<std::time::Duration>,
) -> Result<(), Box<Error>> {
    send_head(stream, head, false)?;
    relay::send_body(stream, body, true, write_timeout).await
}
