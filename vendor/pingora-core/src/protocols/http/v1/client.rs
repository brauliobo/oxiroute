// Copyright 2026 Cloudflare, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! HTTP/1.x client session

/// Header slots a parsed response map has beyond the headers it arrived with, for the request id and the
/// headers route policy adds, so that adding them does not regrow and rehash the map.
const RESPONSE_HEADER_HEADROOM: usize = 3;

use bytes::{BufMut, Bytes, BytesMut};
use futures::FutureExt;
use http::{header, header::AsHeaderName, HeaderValue, StatusCode, Version};
use log::{debug, trace};
use pingora_error::{Error, ErrorType::*, OrErr, Result, RetryType};
use pingora_http::{RequestHeader, ResponseHeader};
use pingora_timeout::timeout;
use std::io::ErrorKind;
use std::str;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::body::{BodyReader, BodyWriter};
use super::common::*;
use crate::protocols::http::HttpTask;
use crate::protocols::{Digest, SocketAddr, Stream, UniqueID, UniqueIDType};
use crate::utils::BufRef;

/// The HTTP 1.x client session
pub struct HttpSession {
    buf: Bytes,
    /// In-progress response header bytes, kept on the session so cancelled reads can resume.
    response_header_read_buf: BytesMut,
    pub(crate) underlying_stream: Stream,
    raw_header: Option<BufRef>,
    preread_body: Option<BufRef>,
    body_reader: BodyReader,
    body_writer: BodyWriter,
    // timeouts:
    /// The read timeout, which will be applied to both reading the header and the body.
    /// The timeout is reset on every read. This is not a timeout on the overall duration of the
    /// response.
    pub read_timeout: Option<Duration>,
    /// Replaces `read_timeout` once the final response header has been read.
    pub body_read_timeout: Option<Duration>,
    /// The write timeout which will be applied to both writing request header and body.
    /// The timeout is reset on every write. This is not a timeout on the overall duration of the
    /// request.
    pub write_timeout: Option<Duration>,
    keepalive_timeout: KeepaliveStatus,
    pub(crate) digest: Box<Digest>,
    response_header: Option<Box<ResponseHeader>>,
    // What keepalive and framing need once `response_header` went to the caller of `read_resp_header_parts`.
    response_facts: Option<ResponseFacts>,
    // What the response header said about its framing when it was parsed.
    response_noted: RequestFacts,
    request_written: Option<RequestMetadata>,
    // Preserve extension and resource lifetime for the legacy owned write API.
    _request_written_owned: Option<Box<RequestHeader>>,
    bytes_sent: usize,
    /// Total response body payload bytes received from upstream
    body_recv: usize,
    // Tracks whether upgrade handshake was successfully completed
    upgraded: bool,
    // Tracks whether downstream request body started sending upgraded bytes
    received_upgrade_req_body: bool,
    // Tracks whether the response read was ever close-delimited
    // (even after body complete)
    close_delimited_resp: bool,
    // If allowed, does not fail with error on invalid content-length
    // (treats as close-delimited response).
    allow_h1_response_invalid_content_length: bool,
    // How the body of a response whose head the caller scanned itself is framed; see `accept_fast_head`.
    fast_framing: Option<FastFraming>,
}

/// How the body of a response is framed, as a caller that scanned the head itself reports it.
#[derive(Clone, Copy, Debug)]
pub struct FastFraming {
    pub chunked: bool,
    pub content_length: Option<usize>,
}

/// What a caller that scanned a response head itself tells the session about it (see
/// [`HttpSession::accept_fast_head`]): the facts the session needs for keepalive and body framing.
#[derive(Clone, Debug)]
pub struct FastHead {
    pub status: StatusCode,
    pub version: Version,
    pub head_len: usize,
    pub connection: Option<HeaderValue>,
    pub transfer_encoding: bool,
    pub framing: FastFraming,
}

/// The parts of a response header the session still reads after the header moved to the caller.
#[derive(Debug)]
struct ResponseFacts {
    status: StatusCode,
    version: Version,
    connection: Option<HeaderValue>,
    keep_alive: Option<HeaderValue>,
    transfer_encoding: bool,
}

impl ResponseFacts {
    fn of(response: &ResponseHeader, noted: RequestFacts) -> Self {
        let connection = noted
            .connection
            .then(|| response.headers.get(header::CONNECTION).cloned())
            .flatten();
        // Only a `Connection: keep-alive` response is asked for its `Keep-Alive` parameters.
        let keep_alive = connection
            .as_ref()
            .and_then(|_| response.headers.get("Keep-Alive").cloned());
        Self {
            status: response.status,
            version: response.version,
            connection,
            keep_alive,
            transfer_encoding: noted.transfer_encoding,
        }
    }
}

#[derive(Debug)]
struct RequestMetadata {
    is_head: bool,
    connection_keepalive: Option<bool>,
    is_upgrade: bool,
}

impl RequestMetadata {
    fn of(req: &RequestHeader, facts: RequestFacts) -> Self {
        Self {
            is_head: req.method == http::method::Method::HEAD,
            connection_keepalive: facts
                .connection
                .then(|| is_buf_keepalive(req.headers.get(header::CONNECTION)))
                .flatten(),
            is_upgrade: facts.upgrade && is_upgrade_req(req),
        }
    }
}

/// HTTP 1.x client session
impl HttpSession {
    /// Create a new http client session from an established (TCP or TLS) [`Stream`].
    pub fn new(stream: Stream) -> Self {
        // TODO: maybe we should put digest in the connection itself
        let digest = Box::new(Digest {
            ssl_digest: stream.get_ssl_digest(),
            timing_digest: stream.get_timing_digest(),
            proxy_digest: stream.get_proxy_digest(),
            socket_digest: stream.get_socket_digest(),
        });
        HttpSession {
            underlying_stream: stream,
            buf: Bytes::new(), // zero size, will be replaced by parsed header later
            response_header_read_buf: BytesMut::new(),
            raw_header: None,
            preread_body: None,
            body_reader: BodyReader::new(true),
            body_writer: BodyWriter::new(),
            keepalive_timeout: KeepaliveStatus::Off,
            response_header: None,
            response_facts: None,
            response_noted: RequestFacts::default(),
            request_written: None,
            _request_written_owned: None,
            read_timeout: None,
            body_read_timeout: None,
            write_timeout: None,
            digest,
            bytes_sent: 0,
            body_recv: 0,
            upgraded: false,
            received_upgrade_req_body: false,
            close_delimited_resp: false,
            allow_h1_response_invalid_content_length: false,
            fast_framing: None,
        }
    }

    /// Create a new http client session and apply peer options
    pub fn new_with_options<P: crate::upstreams::peer::Peer>(stream: Stream, peer: &P) -> Self {
        let mut session = Self::new(stream);
        if let Some(options) = peer.get_peer_options() {
            session.set_allow_h1_response_invalid_content_length(
                options.allow_h1_response_invalid_content_length,
            );
        }
        session
    }

    /// Write the request header to the server
    /// After the request header is sent. The caller can either start reading the response or
    /// sending request body if any.
    pub async fn write_request_header(&mut self, req: Box<RequestHeader>) -> Result<usize> {
        let written = self.write_request_header_ref(&req).await?;
        self._request_written_owned = Some(req);
        Ok(written)
    }

    /// Writes a request head the caller rendered itself, for a request without a body that carries no `Connection`
    /// field: what [`Self::write_request_header_noted()`] leaves behind for such a request.
    pub async fn write_request_head_raw(&mut self, head: &[u8], is_head: bool) -> Result<usize> {
        self.body_writer.init_content_length(0);
        let write_fut = self.underlying_stream.write_all(head);
        match self.write_timeout {
            Some(t) => match timeout(t, write_fut).await {
                Ok(res) => res,
                Err(_) => Err(std::io::Error::from(ErrorKind::TimedOut)),
            },
            None => write_fut.await,
        }
        .map_err(|e| match e.kind() {
            ErrorKind::TimedOut => {
                Error::because(WriteTimedout, "while writing request headers (timeout)", e)
            }
            _ => Error::because(WriteError, "while writing request headers", e),
        })?;
        self.underlying_stream
            .flush()
            .await
            .or_err(WriteError, "flushing request header")?;
        self.request_written = Some(RequestMetadata {
            is_head,
            connection_keepalive: None,
            is_upgrade: false,
        });
        Ok(head.len())
    }

    /// Write a borrowed request header to the server.
    /// After the request header is sent, the caller can either start reading the response or
    /// sending request body if any.
    pub async fn write_request_header_ref(&mut self, req: &RequestHeader) -> Result<usize> {
        self.write_request_header_noted(req, RequestFacts::of_framing(&req.headers))
            .await
    }

    /// Like [`Self::write_request_header_ref()`] for a caller that already knows what `req` says about its
    /// framing and connection handling (see [`RequestFacts`]), so the headers are not looked up again.
    pub async fn write_request_header_noted(
        &mut self,
        req: &RequestHeader,
        facts: RequestFacts,
    ) -> Result<usize> {
        // TODO: make sure this can only be called once
        // init body writer
        self.init_req_body_writer(req, facts);
        let request_metadata = RequestMetadata::of(req, facts);

        let to_wire = http_req_header_to_wire(req).ok_or_else(|| {
            Error::explain(
                InvalidHTTPHeader,
                "request-line contains forbidden delimiter bytes or unsupported HTTP version",
            )
        })?;
        trace!("Writing request header: {to_wire:?}");

        let write_fut = self.underlying_stream.write_all(to_wire.as_ref());
        match self.write_timeout {
            Some(t) => match timeout(t, write_fut).await {
                Ok(res) => res,
                Err(_) => Err(std::io::Error::from(ErrorKind::TimedOut)),
            },
            None => write_fut.await,
        }
        .map_err(|e| match e.kind() {
            ErrorKind::TimedOut => {
                Error::because(WriteTimedout, "while writing request headers (timeout)", e)
            }
            _ => Error::because(WriteError, "while writing request headers", e),
        })?;

        self.underlying_stream
            .flush()
            .await
            .or_err(WriteError, "flushing request header")?;

        // write was successful
        self.request_written = Some(request_metadata);
        Ok(to_wire.len())
    }

    async fn do_write_body(&mut self, buf: &[u8]) -> Result<Option<usize>> {
        let written = self
            .body_writer
            .write_body(&mut self.underlying_stream, buf)
            .await;

        if let Ok(Some(num_bytes)) = written {
            self.bytes_sent += num_bytes;
        }

        written
    }

    /// Write request body. Return Ok(None) if no more body should be written, either due to
    /// Content-Length or the last chunk is already sent
    pub async fn write_body(&mut self, buf: &[u8]) -> Result<Option<usize>> {
        // TODO: verify that request header is sent already
        match self.write_timeout {
            Some(t) => match timeout(t, self.do_write_body(buf)).await {
                Ok(res) => res,
                Err(_) => Error::e_explain(WriteTimedout, format!("writing body, timeout: {t:?}")),
            },
            None => self.do_write_body(buf).await,
        }
    }

    fn maybe_force_close_body_reader(&mut self) {
        if self.upgraded && self.received_upgrade_req_body && !self.body_reader.body_done() {
            // request is done, reset the response body to close
            self.body_reader.init_content_length(0, b"");
        }
    }

    /// Flush local buffer and notify the server by sending the last chunk if chunked encoding is
    /// used.
    pub async fn finish_body(&mut self) -> Result<Option<usize>> {
        let res = self.body_writer.finish(&mut self.underlying_stream).await?;
        self.underlying_stream
            .flush()
            .await
            .or_err(WriteError, "flushing body")?;

        self.maybe_force_close_body_reader();
        Ok(res)
    }

    // Validate the response header read. This function must be called after the response header
    // read.
    fn validate_response(&self) -> Result<()> {
        let resp_header = self
            .response_header
            .as_ref()
            .expect("response header must be read");

        // Validate/reconcile Content-Length per RFC 9110 section 8.6 (hyper
        // parity): identical duplicates and comma-combined identical values are
        // accepted and collapsed. Conflicting or duplicate values are always an
        // unrecoverable framing error. A peer may opt to tolerate a single,
        // otherwise-invalid value and treat the response as close-delimited.
        // Note: Content-Length is already removed if Transfer-Encoding is present.
        if let Err(e) =
            super::common::validate_content_length_facts(self.response_noted, &resp_header.headers)
        {
            if self.allow_h1_response_invalid_content_length
                && super::common::content_length_is_single_token(&resp_header.headers)
            {
                debug!(
                    "tolerating invalid Content-Length ({e}); \
                     treating response as close-delimited"
                );
            } else {
                return Err(e);
            }
        }

        Ok(())
    }

    fn clear_response_header_read_state(&mut self) {
        self.response_header_read_buf.clear();
        self.preread_body = None;
    }

    fn fail_response_header_read<T>(&mut self, e: pingora_error::BError) -> Result<T> {
        self.clear_response_header_read_state();
        Err(e)
    }

    /// Read the response header from the server
    /// This function can be called multiple times, if the headers received are just informational
    /// headers.
    pub async fn read_response(&mut self) -> Result<usize> {
        let mut parse_existing_buffer = true;

        if self.response_header_read_buf.is_empty() {
            // preread_body is set after a completed valid response header is read.
            // If read_response() is called multiple times (i.e. after informational
            // responses), already-read bytes after the 1xx header should be parsed as
            // more response headers. (https://datatracker.ietf.org/doc/html/rfc9110#section-15.2
            // "A 1xx response is terminated by the end of the header section;
            // it cannot contain content or trailers.")
            //
            // Move these bytes into response_header_read_buf so that if parsing the
            // next header is cancelled, a future call can resume without dropping the
            // preread bytes.
            if let Some(preread) = self.preread_body.take().filter(|b| !b.is_empty()) {
                self.response_header_read_buf
                    .put_slice(preread.get(&self.buf));
            } else {
                // If this next read_response call completes successfully, self.buf
                // will be reset to the last response + any body.
                self.buf.clear();
                parse_existing_buffer = false;
            }
        }

        loop {
            if parse_existing_buffer {
                let mut slots = header_slots();
                let mut resp = httparse::Response::new(&mut []);
                let parsed =
                    parse_resp_buffer(&mut resp, &self.response_header_read_buf, &mut slots);
                match parsed {
                    HeaderParseState::Complete(s) => {
                        let total_read = self.response_header_read_buf.len();
                        let base = self.response_header_read_buf.as_ptr() as usize;
                        let mut header_refs = HeaderRefs::new();

                        // Note: resp.headers has the correct number of headers
                        // while header_refs doesn't as it is still empty
                        let _num_headers = populate_headers(base, &mut header_refs, resp.headers);

                        let status = match StatusCode::from_u16(resp.code.unwrap()) {
                            Ok(status) => status,
                            Err(e) => {
                                return self.fail_response_header_read(Error::because(
                                    InvalidHTTPHeader,
                                    "invalid status",
                                    e,
                                ))
                            }
                        };
                        // TODO: enforce https://datatracker.ietf.org/doc/html/rfc9110#section-15.2
                        // "Since HTTP/1.0 did not define any 1xx status codes,
                        // a server MUST NOT send a 1xx response to an HTTP/1.0 client."
                        let version = match resp.version {
                            Some(1) => Version::HTTP_11,
                            Some(0) => Version::HTTP_10,
                            _ => Version::HTTP_09,
                        };
                        // the canonical phrase needs no copy: it is what an unset phrase means
                        let reason = resp
                            .reason
                            .filter(|reason| Some(*reason) != status.canonical_reason())
                            .map(String::from);

                        let buf = std::mem::take(&mut self.response_header_read_buf).freeze();

                        let (response_header, facts) = match parsed_response_header(
                            status,
                            version,
                            reason.as_deref(),
                            header_refs,
                            &buf,
                        ) {
                            Ok(parsed) => parsed,
                            Err(e) => return self.fail_response_header_read(e),
                        };

                        self.raw_header = Some(BufRef(0, s));
                        self.preread_body = Some(BufRef(s, total_read));
                        self.buf = buf;
                        self.response_header = Some(response_header);
                        self.response_noted = facts;
                        if let Err(e) = self.validate_response() {
                            return self.fail_response_header_read(e);
                        }
                        // convert to upgrade body type
                        // https://datatracker.ietf.org/doc/html/rfc9110#status.101
                        // as an "informational" header, this cannot have a body
                        self.upgraded = self
                            .is_upgrade(self.response_header.as_deref().expect("init above"))
                            .unwrap_or(false);
                        // init body reader if upgrade status has changed body mode
                        // (read_response_task will immediately try to init body afterwards anyways)
                        // informational headers will automatically avoid initializing body reader
                        self.init_body_reader();
                        // note that the (request) body writer is converted to close delimit
                        // when the upgraded body tasks are received
                        return Ok(s);
                    }
                    HeaderParseState::Partial => { /* read more below */ }
                    HeaderParseState::Invalid(e) => {
                        return self.fail_response_header_read(Error::because(
                            InvalidHTTPHeader,
                            format!("buf: {}", self.response_header_read_buf.escape_ascii()),
                            e,
                        ));
                    }
                }
            }

            let already_read = self.response_header_read_buf.len();
            if already_read > MAX_HEADER_SIZE {
                /* NOTE: this check only blocks second read. The first large read is allowed
                since the buf is already allocated. The goal is to avoid slowly bloating
                this buffer */
                return self.fail_response_header_read(Error::explain(
                    InvalidHTTPHeader,
                    format!("Response header larger than {MAX_HEADER_SIZE}"),
                ));
            }

            if self
                .response_header_read_buf
                .capacity()
                .saturating_sub(self.response_header_read_buf.len())
                < INIT_HEADER_BUF_SIZE
            {
                self.response_header_read_buf.reserve(INIT_HEADER_BUF_SIZE);
            }

            let read_fut = self
                .underlying_stream
                .read_buf(&mut self.response_header_read_buf);
            let read_result = match self.read_timeout {
                Some(t) => match timeout(t, read_fut).await {
                    Ok(res) => res,
                    Err(_) => {
                        return self.fail_response_header_read(Error::explain(
                            ReadTimedout,
                            "while reading response headers",
                        ));
                    }
                },
                None => read_fut.await,
            };

            match read_result {
                Ok(0) => {
                    let mut e = Error::explain(
                        ConnectionClosed,
                        format!(
                            "while reading response headers, bytes already read: {already_read}",
                        ),
                    );
                    e.retry = RetryType::ReusedOnly;
                    return self.fail_response_header_read(e);
                }
                Ok(_) => {
                    parse_existing_buffer = true;
                }
                Err(e) => {
                    let true_io_error = e.raw_os_error().is_some();
                    let mut e = Error::because(
                        ReadError,
                        format!(
                            "while reading response headers, bytes already read: {already_read}",
                        ),
                        e,
                    );
                    // Likely OSError, typical if a previously reused connection drops it
                    if true_io_error {
                        e.retry = RetryType::ReusedOnly;
                    } // else: not safe to retry TLS error
                    return self.fail_response_header_read(e);
                }
            }
        }
    }

    /// Similar to [`Self::read_response()`], read the response header and then return it. The header moves
    /// to the caller: [`Self::resp_header()`] and [`Self::get_header()`] no longer see it, the session keeps
    /// the status, version and the headers keepalive depends on.
    pub async fn read_resp_header_parts(&mut self) -> Result<Box<ResponseHeader>> {
        self.read_response().await?;
        let header = self
            .response_header
            .take()
            .expect("response header was just read");
        self.response_facts = Some(ResponseFacts::of(&header, self.response_noted));
        Ok(header)
    }

    /// Reads a response head without parsing it: up to and including the first empty line, with the limits,
    /// timeouts and errors of [`Self::read_response()`]. The bytes (and any body bytes that arrived with them)
    /// stay in the session; the caller scans them and then either reports what they say with
    /// [`Self::accept_fast_head()`] or leaves them to [`Self::reparse_raw_head()`].
    pub async fn read_raw_head(&mut self) -> Result<Bytes> {
        let mut buf = BytesMut::with_capacity(INIT_HEADER_BUF_SIZE);
        let mut scanned = 0;
        loop {
            if buf.len() > MAX_HEADER_SIZE {
                return Error::e_explain(
                    InvalidHTTPHeader,
                    format!("Response header larger than {MAX_HEADER_SIZE}"),
                );
            }
            let read_fut = self.underlying_stream.read_buf(&mut buf);
            let read_result = match self.read_timeout {
                Some(t) => timeout(t, read_fut)
                    .await
                    .map_err(|_| Error::explain(ReadTimedout, "while reading response headers"))?,
                None => read_fut.await,
            };
            match read_result {
                Ok(0) => {
                    let mut e = Error::explain(
                        ConnectionClosed,
                        format!(
                            "while reading response headers, bytes already read: {}",
                            buf.len()
                        ),
                    );
                    e.retry = RetryType::ReusedOnly;
                    return Err(e);
                }
                Ok(_) => {}
                Err(e) => {
                    let true_io_error = e.raw_os_error().is_some();
                    let mut e = Error::because(
                        ReadError,
                        format!(
                            "while reading response headers, bytes already read: {}",
                            buf.len()
                        ),
                        e,
                    );
                    if true_io_error {
                        e.retry = RetryType::ReusedOnly;
                    }
                    return Err(e);
                }
            }
            if head_end(&buf, scanned).is_some() {
                self.buf = buf.freeze();
                self.preread_body = None;
                return Ok(self.buf.clone());
            }
            scanned = buf.len();
        }
    }

    /// Leaves the bytes of [`Self::read_raw_head()`] to the parser of [`Self::read_response()`], which reads them
    /// before it reads from the stream.
    pub fn reparse_raw_head(&mut self) {
        self.preread_body = Some(BufRef(0, self.buf.len()));
    }

    /// Takes the facts of a head the caller scanned (the bytes of [`Self::read_raw_head()`]) as the response of
    /// the session, as [`Self::read_response()`] followed by [`Self::read_resp_header_parts()`] would leave it. Only
    /// for a final response that is not an upgrade.
    pub fn accept_fast_head(&mut self, head: FastHead) {
        self.raw_header = Some(BufRef(0, head.head_len));
        self.preread_body = Some(BufRef(head.head_len, self.buf.len()));
        self.response_noted = RequestFacts {
            transfer_encoding: head.transfer_encoding,
            content_lengths: u8::from(head.framing.content_length.is_some()),
            connection: head.connection.is_some(),
            ..RequestFacts::default()
        };
        self.response_facts = Some(ResponseFacts {
            status: head.status,
            version: head.version,
            connection: head.connection,
            keep_alive: None,
            transfer_encoding: head.transfer_encoding,
        });
        self.fast_framing = Some(head.framing);
        self.upgraded = false;
        self.read_timeout = self.body_read_timeout.or(self.read_timeout);
        self.init_body_reader();
    }

    /// Return a reference of the [`ResponseHeader`] if the response is read and not yet taken by
    /// [`Self::read_resp_header_parts()`]
    pub fn resp_header(&self) -> Option<&ResponseHeader> {
        self.response_header.as_deref()
    }

    fn resp_version(&self) -> Option<Version> {
        match (&self.response_header, &self.response_facts) {
            (Some(header), _) => Some(header.version),
            (None, facts) => facts.as_ref().map(|facts| facts.version),
        }
    }

    fn resp_connection(&self) -> Option<&HeaderValue> {
        match (&self.response_header, &self.response_facts) {
            (Some(header), _) => header.headers.get(header::CONNECTION),
            (None, facts) => facts.as_ref().and_then(|facts| facts.connection.as_ref()),
        }
    }

    fn resp_keep_alive(&self) -> Option<&HeaderValue> {
        match (&self.response_header, &self.response_facts) {
            (Some(header), _) => header.headers.get("Keep-Alive"),
            (None, facts) => facts.as_ref().and_then(|facts| facts.keep_alive.as_ref()),
        }
    }

    fn resp_has_transfer_encoding(&self) -> bool {
        match (&self.response_header, &self.response_facts) {
            (Some(header), _) => header.headers.contains_key(header::TRANSFER_ENCODING),
            (None, facts) => facts.as_ref().is_some_and(|facts| facts.transfer_encoding),
        }
    }

    /// Get the header value for the given header name from the response header
    /// If there are multiple headers under the same name, the first one will be returned
    /// Use `self.resp_header().header.get_all(name)` to get all the headers under the same name
    /// Always return `None` if the response is not read yet.
    pub fn get_header(&self, name: impl AsHeaderName) -> Option<&HeaderValue> {
        self.response_header
            .as_ref()
            .and_then(|h| h.headers.get(name))
    }

    /// Get the request header as raw bytes, `b""` when the header doesn't exist or response not read
    pub fn get_header_bytes(&self, name: impl AsHeaderName) -> &[u8] {
        self.get_header(name).map_or(b"", |v| v.as_bytes())
    }

    /// Return the status code of the response if read
    pub fn get_status(&self) -> Option<StatusCode> {
        match (&self.response_header, &self.response_facts) {
            (Some(header), _) => Some(header.status),
            (None, facts) => facts.as_ref().map(|facts| facts.status),
        }
    }

    async fn do_read_body(&mut self) -> Result<Option<BufRef>> {
        self.init_body_reader();
        self.body_reader
            .read_body(&mut self.underlying_stream)
            .await
    }

    async fn do_read_body_bytes(&mut self) -> Result<Option<Bytes>> {
        self.init_body_reader();
        self.body_reader
            .read_body_bytes(&mut self.underlying_stream)
            .await
    }

    /// Read the response body into the internal buffer.
    /// Return `Ok(Some(ref)) after a successful read.
    /// Return `Ok(None)` if there is no more body to read.
    pub async fn read_body_ref(&mut self) -> Result<Option<&[u8]>> {
        let result = match self.read_timeout {
            Some(t) => match timeout(t, self.do_read_body()).await {
                Ok(res) => res,
                Err(_) => Error::e_explain(ReadTimedout, format!("reading body, timeout: {t:?}")),
            },
            None => self.do_read_body().await,
        };

        result.map(|maybe_body| {
            maybe_body.map(|body_ref| {
                let slice = self.body_reader.get_body(&body_ref);
                self.body_recv = self.body_recv.saturating_add(slice.len());
                slice
            })
        })
    }

    /// Similar to [`Self::read_body_ref`] but return `Bytes` instead of a slice reference.
    pub async fn read_body_bytes(&mut self) -> Result<Option<Bytes>> {
        let result = match self.read_timeout {
            Some(t) => match timeout(t, self.do_read_body_bytes()).await {
                Ok(res) => res,
                Err(_) => Error::e_explain(ReadTimedout, format!("reading body, timeout: {t:?}")),
            },
            None => self.do_read_body_bytes().await,
        }?;

        if let Some(body) = &result {
            self.body_recv = self.body_recv.saturating_add(body.len());
        }
        Ok(result)
    }

    /// Upstream response body bytes received (payload only; excludes headers/framing).
    pub fn body_bytes_received(&self) -> usize {
        self.body_recv
    }

    /// Request body bytes written to the upstream (payload only; excludes headers/framing).
    ///
    /// Counts what the body writer accepted, not what the caller offered, so it can be
    /// compared against the request `Content-Length` to detect a truncated request body.
    pub fn body_bytes_sent(&self) -> usize {
        self.bytes_sent
    }

    /// Whether there is no more body to read.
    pub fn is_body_done(&mut self) -> bool {
        self.init_body_reader();
        self.body_reader.body_done()
    }

    pub fn set_allow_h1_response_invalid_content_length(&mut self, allow: bool) {
        self.allow_h1_response_invalid_content_length = allow;
    }

    pub(super) fn get_headers_raw(&self) -> &[u8] {
        // TODO: these get_*() could panic. handle them better
        self.raw_header.as_ref().unwrap().get(&self.buf[..])
    }

    /// Get the raw response header bytes
    pub fn get_headers_raw_bytes(&self) -> Bytes {
        self.raw_header.as_ref().unwrap().get_bytes(&self.buf)
    }

    fn set_keepalive(&mut self, seconds: Option<u64>) {
        match seconds {
            Some(sec) => {
                if sec > 0 {
                    self.keepalive_timeout = KeepaliveStatus::Timeout(Duration::from_secs(sec));
                } else {
                    self.keepalive_timeout = KeepaliveStatus::Infinite;
                }
            }
            None => {
                self.keepalive_timeout = KeepaliveStatus::Off;
            }
        }
    }

    /// Apply keepalive settings according to the server's response
    /// For HTTP 1.1, assume keepalive as long as there is no `Connection: Close` request header.
    /// For HTTP 1.0, only keepalive if there is an explicit header `Connection: keep-alive`.
    pub fn respect_keepalive(&mut self) {
        if self.upgraded || self.get_status() == Some(StatusCode::SWITCHING_PROTOCOLS) {
            // make sure the connection is closed at the end when 101/upgrade is used
            self.set_keepalive(None);
            return;
        }
        if self.body_reader.need_init() || self.close_delimited_resp {
            // Defense-in-depth: response body close-delimited (or no body interpretation
            // upon reuse check)
            // explicitly disable reuse
            self.set_keepalive(None);
            return;
        }
        if self.body_reader.has_bytes_overread() {
            // if more bytes sent than expected, there are likely more bytes coming
            // so don't reuse this connection
            self.set_keepalive(None);
            return;
        }

        // Per [RFC 9112 Section 6.1-16](https://datatracker.ietf.org/doc/html/rfc9112#section-6.1-16),
        // if Transfer-Encoding is received in HTTP/1.0 response, connection MUST be closed after processing.
        if self.resp_version() == Some(Version::HTTP_10) && self.resp_has_transfer_encoding() {
            self.set_keepalive(None);
            return;
        }
        if let Some(keepalive) = self.is_connection_keepalive() {
            if keepalive {
                let (timeout, _max_use) = self.get_keepalive_values();
                // TODO: respect max_use
                match timeout {
                    Some(d) => self.set_keepalive(Some(d)),
                    None => self.set_keepalive(Some(0)), // infinite
                }
            } else {
                self.set_keepalive(None);
            }
        } else if self.resp_version() == Some(Version::HTTP_11) {
            self.set_keepalive(Some(0)); // on by default for http 1.1
        } else {
            self.set_keepalive(None); // off by default for http 1.0
        }
    }

    // Whether this session will be kept alive
    pub fn will_keepalive(&self) -> bool {
        !matches!(self.keepalive_timeout, KeepaliveStatus::Off)
    }

    fn is_connection_keepalive(&self) -> Option<bool> {
        let request_keepalive = self
            .request_written
            .as_ref()
            .and_then(|req| req.connection_keepalive);

        match request_keepalive {
            // ignore what the server sends if request disables keepalive explicitly
            Some(false) => Some(false),
            _ => is_buf_keepalive(self.resp_connection()),
        }
    }

    /// `Keep-Alive: timeout=5, max=1000` => 5, 1000
    /// This is defined in the below spec, this not part of any RFC, so
    /// it's behavior is different on different platforms.
    /// https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Keep-Alive
    fn get_keepalive_values(&self) -> (Option<u64>, Option<usize>) {
        let Some(keep_alive_header) = self.resp_keep_alive() else {
            return (None, None);
        };

        let Ok(header_value) = str::from_utf8(keep_alive_header.as_bytes()) else {
            return (None, None);
        };

        let mut timeout = None;
        let mut max = None;

        for param in header_value.split(',') {
            let parts = param.split_once('=').map(|(k, v)| (k.trim(), v));
            match parts {
                Some(("timeout", timeout_value)) => timeout = timeout_value.trim().parse().ok(),
                Some(("max", max_value)) => max = max_value.trim().parse().ok(),
                _ => {}
            }
        }

        (timeout, max)
    }

    /// Close the connection abruptly. This allows to signal the server that the connection is closed
    /// before dropping [`HttpSession`]
    pub async fn shutdown(&mut self) {
        let _ = self.underlying_stream.shutdown().await;
    }

    /// Consume `self`, if the connection can be reused, the underlying stream will be returned.
    /// The returned connection can be kept in a connection pool so that next time the same
    /// server is being contacted. A new client session can be created via [`Self::new()`].
    /// If the connection cannot be reused, the underlying stream will be closed and `None` will be
    /// returned.
    pub async fn reuse(mut self) -> Option<Stream> {
        if !self.body_reader.body_complete() || self.body_reader.has_bytes_overread() {
            self.set_keepalive(None);
        }

        // TODO: this function is unnecessarily slow for keepalive case
        // because that case does not need async
        match self.keepalive_timeout {
            KeepaliveStatus::Off => {
                debug!("HTTP shutdown connection");
                self.shutdown().await;
                None
            }
            _ => Some(self.underlying_stream),
        }
    }

    fn init_body_reader(&mut self) {
        if self.body_reader.need_init() {
            // follow https://datatracker.ietf.org/doc/html/rfc9112#section-6.3
            let preread_body = self.preread_body.as_ref().unwrap().get_bytes(&self.buf);

            let upgraded = if let Some(code) = self.get_status() {
                match code.as_u16() {
                    101 => self.is_upgrade_req(),
                    100..=199 => {
                        // informational headers, not enough to init body reader
                        return;
                    }
                    204 | 304 => {
                        // no body by definition
                        self.body_reader.init_content_length(0, &preread_body);
                        return;
                    }
                    _ => false,
                }
            } else {
                false
            };

            if self.request_written.as_ref().is_some_and(|req| req.is_head) {
                self.body_reader.init_content_length(0, &preread_body);
                return;
            }

            if upgraded {
                self.body_reader.init_close_delimited_preread(preread_body);
                self.close_delimited_resp = true;
            } else if self.is_chunked_encoding() {
                // if chunked encoding, content-length should be ignored
                self.body_reader.init_chunked(&preread_body);
            } else if let Some(cl) = match self.fast_framing {
                Some(framing) => framing.content_length,
                None => (self.response_noted.content_lengths > 0)
                    .then(|| self.get_content_length().unwrap_or(None))
                    .flatten(),
            } {
                self.body_reader
                    .init_content_length_preread(cl, preread_body);
            } else {
                self.body_reader.init_close_delimited_preread(preread_body);
                self.close_delimited_resp = true;
            }
        }
    }

    /// Whether this request is for upgrade
    pub fn is_upgrade_req(&self) -> bool {
        self.request_written
            .as_ref()
            .is_some_and(|req| req.is_upgrade)
    }

    /// `Some(true)` if the this is a successful upgrade
    /// `Some(false)` if the request is an upgrade but the response refuses it
    /// `None` if the request is not an upgrade.
    fn is_upgrade(&self, header: &ResponseHeader) -> Option<bool> {
        if self.is_upgrade_req() {
            Some(is_upgrade_resp(header))
        } else {
            None
        }
    }

    /// Was this request successfully turned into an upgraded connection?
    ///
    /// Both the request had to have been an `Upgrade` request
    /// and the response had to have been a `101 Switching Protocols`.
    pub fn was_upgraded(&self) -> bool {
        self.upgraded
    }

    /// If upgraded but not yet converted, then body writer will be
    /// converted to http1.0 mode (pass through bytes as-is).
    pub fn maybe_upgrade_body_writer(&mut self) {
        if self.was_upgraded() {
            self.received_upgrade_req_body = true;
            self.body_writer.convert_to_close_delimited();
        }
    }

    fn get_content_length(&self) -> Result<Option<usize>> {
        match self.resp_header() {
            Some(h) => content_length_for_framing(&h.headers),
            None => Ok(None),
        }
    }

    fn is_chunked_encoding(&self) -> bool {
        if let Some(framing) = self.fast_framing {
            return framing.chunked;
        }
        self.response_noted.transfer_encoding
            && self
                .resp_header()
                .map(|h| is_chunked_encoding_from_headers(&h.headers))
                .unwrap_or(false)
    }

    fn init_req_body_writer(&mut self, header: &RequestHeader, facts: RequestFacts) {
        let headers = &header.headers;
        if facts.transfer_encoding && is_chunked_encoding_from_headers(headers) {
            // transfer-encoding takes priority over content-length
            self.body_writer.init_chunked();
            return;
        }
        // Resolve Content-Length through the shared, range-checked helper so the
        // value is reconciled (identical duplicates / comma lists) and never
        // truncated. Per RFC 9112, requests are never close-delimited: an absent
        // or unusable Content-Length means a zero-length body. Headers are
        // expected to be validated on read.
        let framing = if facts.content_lengths > 0 {
            content_length_for_framing(headers)
        } else {
            Ok(None)
        };
        let content_length = framing.unwrap_or_else(|e| {
            // Headers are validated on read, so this is not expected here;
            // log the fallback to a zero-length body rather than failing
            // silently if validation was somehow bypassed.
            debug!("invalid Content-Length while writing request; framing zero-length body: {e}");
            None
        });
        self.body_writer
            .init_content_length(content_length.unwrap_or(0));
    }

    // should (continue to) try to read response header or start reading response body
    fn should_read_resp_header(&self) -> bool {
        match self.get_status().map(|s| s.as_u16()) {
            Some(101) => false,      // switching protocol successful, no more header to read
            Some(100..=199) => true, // only informational header read
            Some(_) => false,
            None => true, // no response code, no header read yet
        }
    }

    pub async fn read_response_task(&mut self) -> Result<HttpTask> {
        if self.should_read_resp_header() {
            let resp_header = self.read_resp_header_parts().await?;
            let end_of_body = self.is_body_done();
            debug!("Response header: {resp_header:?}");
            if !resp_header.status.is_informational() {
                self.read_timeout = self.body_read_timeout.or(self.read_timeout);
            }
            trace!(
                "Raw Response header: {:?}",
                str::from_utf8(self.get_headers_raw()).unwrap()
            );
            Ok(HttpTask::Header(resp_header, end_of_body))
        } else if self.is_body_done() {
            // no body
            debug!("Response is done");
            Ok(HttpTask::Done)
        } else {
            /* need to read body */
            let body = self.read_body_bytes().await?;
            Ok(self.body_task(body))
        }
        // TODO: support h1 trailer
    }

    /// The next response task if it needs no wait for the network: the end of a response, or a body chunk the
    /// connection holds already or that a read returns at once. Where [`Self::read_response_task`] would wait this
    /// returns `None`, without arming a read timeout; a response header is always left to `read_response_task`.
    pub fn try_read_response_task(&mut self) -> Option<Result<HttpTask>> {
        if self.should_read_resp_header() {
            return None;
        }
        if self.is_body_done() {
            return Some(Ok(HttpTask::Done));
        }
        let body = self.do_read_body_bytes().now_or_never()?;
        Some(body.map(|body| {
            if let Some(body) = &body {
                self.body_recv = self.body_recv.saturating_add(body.len());
            }
            self.body_task(body)
        }))
    }

    fn body_task(&mut self, body: Option<Bytes>) -> HttpTask {
        let end_of_body = self.is_body_done();
        debug!(
            "Response body: {} bytes, end: {end_of_body}",
            body.as_ref().map_or(0, |b| b.len())
        );
        trace!("Response body: {body:?}, upgraded: {}", self.upgraded);
        if self.upgraded {
            HttpTask::UpgradedBody(body, end_of_body)
        } else {
            HttpTask::Body(body, end_of_body)
        }
    }

    /// Return the [Digest] of the connection
    ///
    /// For reused connection, the timing in the digest will reflect its initial handshakes
    /// The caller should check if the connection is reused to avoid misuse the timing field.
    pub fn digest(&self) -> &Digest {
        &self.digest
    }

    /// Return a mutable [Digest] reference for the connection.
    pub fn digest_mut(&mut self) -> &mut Digest {
        &mut self.digest
    }

    /// Return the server (peer) address recorded in the connection digest.
    pub fn server_addr(&self) -> Option<&SocketAddr> {
        self.digest()
            .socket_digest
            .as_ref()
            .map(|d| d.peer_addr())?
    }

    /// Return the client (local) address recorded in the connection digest.
    pub fn client_addr(&self) -> Option<&SocketAddr> {
        self.digest()
            .socket_digest
            .as_ref()
            .map(|d| d.local_addr())?
    }

    /// Get the reference of the [Stream] that this HTTP session is operating upon.
    pub fn stream(&self) -> &Stream {
        &self.underlying_stream
    }

    /// Consume `self`, the underlying [Stream] will be returned and can be used
    /// directly, for example, in the case of HTTP upgrade. It is not flushed
    /// prior to being returned.
    pub fn into_inner(self) -> Stream {
        self.underlying_stream
    }
}

#[inline]
fn parse_resp_buffer<'buf, 'h>(
    resp: &mut httparse::Response<'h, 'buf>,
    buf: &'buf [u8],
    slots: &'h mut HeaderSlots<'buf>,
) -> HeaderParseState {
    let mut parser = httparse::ParserConfig::default();
    parser.allow_spaces_after_header_name_in_responses(true);
    parser.allow_obsolete_multiline_headers_in_responses(true);
    let res = match parser.parse_response_with_uninit_headers(resp, buf, slots) {
        Ok(s) => s,
        Err(e) => {
            return HeaderParseState::Invalid(e);
        }
    };
    match res {
        httparse::Status::Complete(s) => HeaderParseState::Complete(s),
        _ => HeaderParseState::Partial,
    }
}

/// The index after the first empty line of `buf` that ends at or after `from - 2`, for a head whose lines end in
/// CRLF or a bare LF.
fn head_end(buf: &[u8], from: usize) -> Option<usize> {
    let mut at = from.saturating_sub(2);
    while let Some(offset) = buf.get(at..)?.iter().position(|byte| *byte == b'\n') {
        let line_end = at + offset;
        match (buf.get(line_end + 1), buf.get(line_end + 2)) {
            (Some(b'\n'), _) => return Some(line_end + 2),
            (Some(b'\r'), Some(b'\n')) => return Some(line_end + 3),
            _ => at = line_end + 1,
        }
    }
    None
}

/// The response header of a parsed head, and what its headers say about framing and connection handling. The
/// values stay slices of `buf`, no header value is copied.
fn parsed_response_header(
    status: StatusCode,
    version: Version,
    reason: Option<&str>,
    header_refs: HeaderRefs,
    buf: &Bytes,
) -> Result<(Box<ResponseHeader>, RequestFacts)> {
    // `ResponseHeader` does not hand out its header map mutably (pingora-http 0.9): the map is filled first
    let mut parts = http::Response::new(()).into_parts().0;
    parts.status = status;
    parts.version = version;
    parts
        .headers
        .reserve(header_refs.len() + RESPONSE_HEADER_HEADROOM);

    let mut facts = RequestFacts::default();
    for header in header_refs {
        let value_bytes = header.get_value_bytes(buf);
        let header_value = if is_header_value(&value_bytes) {
            // httparse accepts some bytes that are not valid HeaderValue contents, so the value is checked
            // above; it is a slice of the read buffer.
            unsafe { HeaderValue::from_maybe_shared_unchecked(value_bytes) }
        } else {
            folded_header_value(value_bytes)?
        };
        append_parsed_request_header(
            &mut parts.headers,
            header.get_name(buf),
            header_value,
            &mut facts,
        )
        .or_err(InvalidHTTPHeader, "while parsing request header")?;
    }

    // Transfer encoding overrides content length, so when
    // both are present, we MUST remove content length. This is
    // https://datatracker.ietf.org/doc/html/rfc9112#section-6.3-2.3
    if facts.content_lengths > 0 && facts.transfer_encoding {
        parts.headers.remove(header::CONTENT_LENGTH);
        facts.content_lengths = 0;
    }

    let mut response_header = Box::new(ResponseHeader::from(parts));
    response_header.set_reason_phrase(reason)?;
    Ok((response_header, facts))
}

/// A value with obsolete line folding, or a stray CR, LF or NUL: normalized the way pingora-http does (each fold
/// becomes one space). Any other byte `HeaderValue` rejects is an error.
fn folded_header_value(value: Bytes) -> Result<HeaderValue> {
    if !value.iter().all(|byte| {
        matches!(byte, b'\r' | b'\n' | 0) || is_header_value(std::slice::from_ref(byte))
    }) {
        return Error::e_explain(InvalidHTTPHeader, "invalid response header value");
    }
    Ok(pingora_http::header_value_from_raw(value))
}

/// Returns `true` if `path` contains a byte that must never appear in an HTTP/1.1
/// request target: NUL (`0x00`), LF (`0x0a`), CR (`0x0d`), or SP (`0x20`).
///
/// See RFC 9112 §3.2 (request-target) and RFC 3986 §2.1 (URI characters).
/// These bytes are also forbidden in HTTP/2 `:path` per RFC 7540 §8.1.2.3.
#[inline]
pub(crate) fn request_target_has_forbidden_byte(path: &[u8]) -> bool {
    path.iter().any(|&b| matches!(b, 0x00 | 0x0a | 0x0d | 0x20))
}

// TODO: change it to to_buf
#[inline]
pub fn http_req_header_to_wire(req: &RequestHeader) -> Option<BytesMut> {
    let mut buf = BytesMut::with_capacity(512);

    // Request-Line
    let method = req.method.as_str().as_bytes();
    buf.put_slice(method);
    buf.put_u8(b' ');

    let path = req.raw_path();
    if request_target_has_forbidden_byte(path) {
        return None;
    }
    buf.put_slice(path);
    buf.put_u8(b' ');

    let version = match req.version {
        Version::HTTP_09 => "HTTP/0.9",
        Version::HTTP_10 => "HTTP/1.0",
        Version::HTTP_11 => "HTTP/1.1",
        Version::HTTP_2 => "HTTP/2",
        _ => {
            return None; /*TODO: unsupported version */
        }
    };
    buf.put_slice(version.as_bytes());
    buf.put_slice(CRLF);

    // headers
    req.header_to_h1_wire(&mut buf);
    buf.put_slice(CRLF);
    Some(buf)
}

impl UniqueID for HttpSession {
    fn id(&self) -> UniqueIDType {
        self.underlying_stream.id()
    }
}

#[cfg(test)]
mod tests_stream {
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };

    use super::*;
    use crate::protocols::http::v1::body::{BodyMode, ParseState};
    use crate::upstreams::peer::PeerOptions;
    use crate::ErrorType;
    use rstest::rstest;
    use tokio_test::io::Builder;

    fn init_log() {
        let _ = env_logger::builder().is_test(true).try_init();
    }

    struct CloneProbe(Arc<AtomicUsize>);

    impl Clone for CloneProbe {
        fn clone(&self) -> Self {
            self.0.fetch_add(1, Ordering::Relaxed);
            Self(Arc::clone(&self.0))
        }
    }

    #[derive(Clone)]
    struct DropProbe(Arc<AtomicBool>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    fn fast(head: &[u8], framing: FastFraming, connection: Option<&'static str>) -> FastHead {
        FastHead {
            status: StatusCode::OK,
            version: Version::HTTP_11,
            head_len: head.len(),
            connection: connection.map(HeaderValue::from_static),
            transfer_encoding: framing.chunked,
            framing,
        }
    }

    #[tokio::test]
    async fn a_scanned_head_leaves_the_body_to_the_session() {
        let head = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n";
        let input = [&head[..], b"hello"].concat();
        let mut http_stream = HttpSession::new(Box::new(Builder::new().read(&input).build()));
        let raw = http_stream.read_raw_head().await.unwrap();
        assert_eq!(&raw[..head.len()], head);
        let framing = FastFraming {
            chunked: false,
            content_length: Some(5),
        };
        http_stream.accept_fast_head(fast(head, framing, None));
        assert_eq!(
            http_stream.read_body_bytes().await.unwrap().unwrap(),
            "hello"
        );
        assert!(http_stream.is_body_done());
        http_stream.respect_keepalive();
        assert!(http_stream.will_keepalive());
    }

    #[tokio::test]
    async fn a_scanned_chunked_head_is_read_as_chunked() {
        let head = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
        let input = [&head[..], b"3\r\nabc\r\n0\r\n\r\n"].concat();
        let mut http_stream = HttpSession::new(Box::new(Builder::new().read(&input).build()));
        http_stream.read_raw_head().await.unwrap();
        let framing = FastFraming {
            chunked: true,
            content_length: None,
        };
        http_stream.accept_fast_head(fast(head, framing, None));
        assert_eq!(http_stream.read_body_bytes().await.unwrap().unwrap(), "abc");
        assert!(http_stream.read_body_bytes().await.unwrap().is_none());
        assert!(http_stream.is_body_done());
    }

    #[tokio::test]
    async fn a_scanned_connection_close_ends_keepalive() {
        let head = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let mut http_stream = HttpSession::new(Box::new(Builder::new().read(head).build()));
        http_stream.read_raw_head().await.unwrap();
        let framing = FastFraming {
            chunked: false,
            content_length: Some(0),
        };
        http_stream.accept_fast_head(fast(head, framing, Some("close")));
        assert!(http_stream.read_body_bytes().await.unwrap().is_none());
        http_stream.respect_keepalive();
        assert!(!http_stream.will_keepalive());
    }

    #[tokio::test]
    async fn a_raw_head_the_classic_parser_reads_gives_the_same_response() {
        let input = b"HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\nX-A: 1\r\n\r\nno";
        let mut http_stream = HttpSession::new(Box::new(Builder::new().read(input).build()));
        http_stream.read_raw_head().await.unwrap();
        http_stream.reparse_raw_head();
        let header = http_stream.read_resp_header_parts().await.unwrap();
        assert_eq!(header.status, 404);
        assert_eq!(header.headers["x-a"], "1");
        assert_eq!(http_stream.read_body_bytes().await.unwrap().unwrap(), "no");
    }

    #[tokio::test]
    async fn a_raw_request_head_is_written_as_given_and_a_head_request_has_no_body() {
        let request = b"HEAD / HTTP/1.1\r\nhost: a\r\n\r\n";
        let mock = Builder::new()
            .write(request)
            .read(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\n")
            .build();
        let mut http_stream = HttpSession::new(Box::new(mock));
        assert_eq!(
            http_stream
                .write_request_head_raw(request, true)
                .await
                .unwrap(),
            request.len()
        );
        http_stream.read_response().await.unwrap();
        http_stream.respect_keepalive();
        assert!(http_stream.read_body_bytes().await.unwrap().is_none());
        assert!(http_stream.is_body_done());
    }

    #[tokio::test]
    async fn read_basic_response() {
        init_log();
        let input = b"HTTP/1.1 200 OK\r\n\r\n";
        let mock_io = Builder::new().read(&input[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let res = http_stream.read_response().await;
        assert_eq!(input.len(), res.unwrap());
        assert_eq!(0, http_stream.resp_header().unwrap().headers.len());
    }

    #[tokio::test]
    async fn parsed_response_omits_case_map_and_normalizes_h1_wire_names() {
        let input = b"HTTP/1.1 200 OK\r\n\
            sEt-CoOkIe: a=1; Path=/\r\n\
            SET-cOOkie: b=2; Path=/\r\n\
            X-MiXeD: retained\r\n\
            cAcHe-CoNtRoL: public, max-age=60\r\n\
            CoNnEcTiOn: x-hop\r\n\
            X-HoP: secret\r\n\
            cOnTeNt-LeNgTh: 0\r\n\r\n";
        let mock_io = Builder::new().read(input).build();
        let mut session = HttpSession::new(Box::new(mock_io));
        session.read_response().await.unwrap();

        let buffer_start = session.buf.as_ptr() as usize;
        let buffer_end = buffer_start + session.buf.len();
        let response = session.response_header.as_mut().unwrap();
        assert!(!response.has_case());
        assert_eq!(
            response
                .headers
                .get_all(header::SET_COOKIE)
                .iter()
                .map(HeaderValue::as_bytes)
                .collect::<Vec<_>>(),
            [b"a=1; Path=/".as_slice(), b"b=2; Path=/".as_slice()]
        );
        for value in response.headers.values() {
            let value_start = value.as_bytes().as_ptr() as usize;
            assert!((buffer_start..buffer_end).contains(&value_start));
        }

        response.remove_header(&header::CONNECTION);
        response.remove_header("x-hop");
        let mut wire = Vec::new();
        response.header_to_h1_wire(&mut wire);
        assert_eq!(
            wire,
            b"Set-Cookie: a=1; Path=/\r\n\
              Set-Cookie: b=2; Path=/\r\n\
              x-mixed: retained\r\n\
              Cache-Control: public, max-age=60\r\n\
              Content-Length: 0\r\n"
        );
    }

    #[tokio::test]
    async fn read_response_custom_reason() {
        init_log();
        let input = b"HTTP/1.1 200 Just Fine\r\n\r\n";
        let mock_io = Builder::new().read(&input[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let res = http_stream.read_response().await;
        assert_eq!(input.len(), res.unwrap());
        assert_eq!(
            http_stream.resp_header().unwrap().get_reason_phrase(),
            Some("Just Fine")
        );
    }

    #[tokio::test]
    async fn read_response_default() {
        init_log();
        let input_header = b"HTTP/1.1 200 OK\r\n\r\n";
        let input_body = b"abc";
        let input_close = b""; // simulating close
        let mock_io = Builder::new()
            .read(&input_header[..])
            .read(&input_body[..])
            .read(&input_close[..])
            .build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let res = http_stream.read_response().await;
        assert_eq!(input_header.len(), res.unwrap());
        let res = http_stream.read_body_ref().await.unwrap();
        assert_eq!(res.unwrap(), input_body);
        assert_eq!(
            http_stream.body_reader.body_state,
            ParseState::UntilClose(3)
        );
        let res = http_stream.read_body_ref().await.unwrap();
        assert_eq!(res, None);
        assert_eq!(http_stream.body_reader.body_state, ParseState::Complete(3));
    }

    #[tokio::test]
    async fn body_bytes_received_content_length() {
        init_log();
        let input_header = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\n";
        let input_body = b"abc";
        let input_close = b""; // simulating close
        let mock_io = Builder::new()
            .read(&input_header[..])
            .read(&input_body[..])
            .read(&input_close[..])
            .build();
        let mut http = HttpSession::new(Box::new(mock_io));
        http.read_response().await.unwrap();
        let _ = http.read_body_ref().await.unwrap();
        let _ = http.read_body_ref().await.unwrap();
        assert_eq!(http.body_bytes_received(), 3);
    }

    #[tokio::test]
    async fn body_bytes_received_chunked() {
        init_log();
        let input_header = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
        let input_body = b"3\r\nabc\r\n0\r\n\r\n";
        let mock_io = Builder::new()
            .read(&input_header[..])
            .read(&input_body[..])
            .build();
        let mut http = HttpSession::new(Box::new(mock_io));
        http.read_response().await.unwrap();
        // first read returns the payload chunk
        let first = http.read_body_ref().await.unwrap();
        assert_eq!(first.unwrap(), b"abc");
        // next read consumes terminating chunk
        let _ = http.read_body_ref().await.unwrap();
        assert_eq!(http.body_bytes_received(), 3);
    }

    #[tokio::test]
    async fn h1_body_bytes_received_http10_until_close() {
        init_log();
        let header = b"HTTP/1.1 200 OK\r\n\r\n";
        let body = b"abc";
        let close = b"";
        let mock = Builder::new()
            .read(&header[..])
            .read(&body[..])
            .read(&close[..])
            .build();
        let mut http = HttpSession::new(Box::new(mock));
        http.read_response().await.unwrap();
        let _ = http.read_body_ref().await.unwrap();
        let _ = http.read_body_ref().await.unwrap();
        assert_eq!(http.body_bytes_received(), 3);
    }

    #[tokio::test]
    async fn h1_body_bytes_received_chunked_multi() {
        init_log();
        let header = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
        let body = b"1\r\na\r\n2\r\nbc\r\n0\r\n\r\n"; // payload abc
        let mock = Builder::new().read(&header[..]).read(&body[..]).build();
        let mut http = HttpSession::new(Box::new(mock));
        http.read_response().await.unwrap();
        // first chunk
        let s1 = http.read_body_ref().await.unwrap().unwrap();
        assert_eq!(s1, b"a");
        // second chunk
        let s2 = http.read_body_ref().await.unwrap().unwrap();
        assert_eq!(s2, b"bc");
        // end
        let _ = http.read_body_ref().await.unwrap();
        assert_eq!(http.body_bytes_received(), 3);
    }

    #[tokio::test]
    async fn h1_body_bytes_received_preread_in_header_buf() {
        init_log();
        // header and a small body arrive together
        let combined = b"HTTP/1.1 200 OK\r\n\r\nabc";
        let close = b"";
        let mock = Builder::new().read(&combined[..]).read(&close[..]).build();
        let mut http = HttpSession::new(Box::new(mock));
        http.read_response().await.unwrap();
        // first body read should return the preread bytes
        let s = http.read_body_ref().await.unwrap().unwrap();
        assert_eq!(s, b"abc");
        // then EOF
        let _ = http.read_body_ref().await.unwrap();
        assert_eq!(http.body_bytes_received(), 3);
    }

    #[tokio::test]
    async fn content_length_preread_body_task_shares_header_allocation() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabc";
        let body_offset = response.len() - 3;
        let mock = Builder::new().read(response).build();
        let mut http = HttpSession::new(Box::new(mock));

        assert!(matches!(
            http.read_response_task().await.unwrap(),
            HttpTask::Header(_, false)
        ));
        let expected_ptr = http.buf.as_ptr().wrapping_add(body_offset);
        match http.read_response_task().await.unwrap() {
            HttpTask::Body(Some(body), true) => {
                assert_eq!(body, b"abc"[..]);
                assert_eq!(body.as_ptr(), expected_ptr);
            }
            task => panic!("expected final preread body, got {task:?}"),
        }
        assert!(http.body_reader.body_buf.is_none());
    }

    #[tokio::test]
    async fn content_length_partial_preread_continues_with_socket_read() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nab";
        let body_offset = response.len() - 2;
        let mock = Builder::new().read(response).read(b"c").build();
        let mut http = HttpSession::new(Box::new(mock));

        assert!(matches!(
            http.read_response_task().await.unwrap(),
            HttpTask::Header(_, false)
        ));
        let expected_ptr = http.buf.as_ptr().wrapping_add(body_offset);
        match http.read_response_task().await.unwrap() {
            HttpTask::Body(Some(body), false) => {
                assert_eq!(body, b"ab"[..]);
                assert_eq!(body.as_ptr(), expected_ptr);
            }
            task => panic!("expected partial preread body, got {task:?}"),
        }
        assert!(matches!(
            http.read_response_task().await.unwrap(),
            HttpTask::Body(Some(body), true) if body == b"c"[..]
        ));
        assert_eq!(http.body_bytes_received(), 3);
    }

    #[tokio::test]
    async fn content_length_preread_overread_disables_reuse() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nabc";
        let body_offset = response.len() - 3;
        let mock = Builder::new().read(response).build();
        let mut http = HttpSession::new(Box::new(mock));

        assert!(matches!(
            http.read_response_task().await.unwrap(),
            HttpTask::Header(_, false)
        ));
        let expected_ptr = http.buf.as_ptr().wrapping_add(body_offset);
        match http.read_response_task().await.unwrap() {
            HttpTask::Body(Some(body), true) => {
                assert_eq!(body, b"ab"[..]);
                assert_eq!(body.as_ptr(), expected_ptr);
            }
            task => panic!("expected truncated preread body, got {task:?}"),
        }
        assert_eq!(http.body_reader.get_body_overread(), Some(&b"c"[..]));
        http.respect_keepalive();
        assert!(!http.will_keepalive());
    }

    #[tokio::test]
    async fn close_delimited_preread_shares_header_allocation() {
        let response = b"HTTP/1.1 200 OK\r\n\r\nabc";
        let body_offset = response.len() - 3;
        let mock = Builder::new().read(response).read(b"").build();
        let mut http = HttpSession::new(Box::new(mock));

        http.read_response().await.unwrap();
        let expected_ptr = http.buf.as_ptr().wrapping_add(body_offset);
        let body = http.read_body_bytes().await.unwrap().unwrap();
        assert_eq!(body, b"abc"[..]);
        assert_eq!(body.as_ptr(), expected_ptr);
        assert!(http.read_body_bytes().await.unwrap().is_none());
        assert_eq!(http.body_reader.body_state, ParseState::Complete(3));
    }

    #[tokio::test]
    async fn h1_body_bytes_received_overread_content_length() {
        init_log();
        let header1 = b"HTTP/1.1 200 OK\r\n";
        let header2 = b"Content-Length: 2\r\n\r\n";
        let body = b"abc"; // one extra byte beyond CL
        let mock = Builder::new()
            .read(&header1[..])
            .read(&header2[..])
            .read(&body[..])
            .build();
        let mut http = HttpSession::new(Box::new(mock));
        http.read_response().await.unwrap();
        let s = http.read_body_ref().await.unwrap().unwrap();
        assert_eq!(s, b"ab");
        // then end
        let _ = http.read_body_ref().await.unwrap();
        assert_eq!(http.body_bytes_received(), 2);
    }

    #[tokio::test]
    async fn h1_body_bytes_received_after_100_continue() {
        init_log();
        let info = b"HTTP/1.1 100 Continue\r\n\r\n";
        let header = b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\n";
        let body = b"x";
        let mock = Builder::new()
            .read(&info[..])
            .read(&header[..])
            .read(&body[..])
            .build();
        let mut http = HttpSession::new(Box::new(mock));
        // read informational
        match http.read_response_task().await.unwrap() {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 100);
                assert!(!eob);
            }
            _ => panic!("expected informational header"),
        }
        // read final header
        match http.read_response_task().await.unwrap() {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 200);
                assert!(!eob);
            }
            _ => panic!("expected final header"),
        }
        // read body
        let s = http.read_body_ref().await.unwrap().unwrap();
        assert_eq!(s, b"x");
        let _ = http.read_body_ref().await.unwrap();
        assert_eq!(http.body_bytes_received(), 1);
    }

    #[tokio::test]
    async fn read_response_overread() {
        init_log();
        let input_header = b"HTTP/1.1 200 OK\r\n";
        let input_header2 = b"Content-Length: 2\r\n\r\n";
        let input_body = b"abc";
        let mock_io = Builder::new()
            .read(&input_header[..])
            .read(&input_header2[..])
            .read(&input_body[..])
            .build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let res = http_stream.read_response().await;
        assert_eq!(input_header.len() + input_header2.len(), res.unwrap());
        let res = http_stream.read_body_ref().await.unwrap();
        assert_eq!(res.unwrap(), &input_body[..2]);
        assert_eq!(http_stream.body_reader.body_state, ParseState::Complete(2));
        let res = http_stream.read_body_ref().await.unwrap();
        assert_eq!(res, None);
        assert_eq!(http_stream.body_reader.body_state, ParseState::Complete(2));
        http_stream.respect_keepalive();
        assert!(!http_stream.will_keepalive());
    }

    #[tokio::test]
    async fn read_resp_header_with_space() {
        init_log();
        let input = b"HTTP/1.1 200 OK\r\nServer : pingora\r\n\r\n";
        let mock_io = Builder::new().read(&input[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let res = http_stream.read_response().await;
        assert_eq!(input.len(), res.unwrap());
        assert_eq!(1, http_stream.resp_header().unwrap().headers.len());
        assert_eq!(http_stream.get_header("Server").unwrap(), "pingora");
    }

    #[cfg(feature = "patched_http1")]
    #[tokio::test]
    async fn read_resp_header_with_utf8() {
        init_log();
        let input = "HTTP/1.1 200 OK\r\nServer👍: pingora\r\n\r\n".as_bytes();
        let mock_io = Builder::new().read(input).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let resp = http_stream.read_resp_header_parts().await.unwrap();
        assert_eq!(1, http_stream.resp_header().unwrap().headers.len());
        assert_eq!(http_stream.get_header("Server👍").unwrap(), "pingora");
        assert_eq!(resp.headers.get("Server👍").unwrap(), "pingora");
    }

    #[tokio::test]
    #[should_panic(expected = "There is still data left to read.")]
    async fn read_timeout() {
        init_log();
        let input = b"HTTP/1.1 200 OK\r\n\r\n";
        let mock_io = Builder::new()
            .wait(Duration::from_secs(2))
            .read(&input[..])
            .build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        http_stream.read_timeout = Some(Duration::from_secs(1));
        let res = http_stream.read_response().await;
        assert_eq!(res.unwrap_err().etype(), &ErrorType::ReadTimedout);
    }

    #[tokio::test]
    async fn try_read_response_task_returns_only_what_needs_no_wait() {
        init_log();
        let head = b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nabc";
        let rest = b"def";
        let mock_io = Builder::new()
            .read(&head[..])
            .wait(Duration::from_millis(50))
            .read(&rest[..])
            .build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        http_stream.read_timeout = Some(Duration::from_secs(5));
        // A header is left to `read_response_task`.
        assert!(http_stream.try_read_response_task().is_none());
        let task = http_stream.read_response_task().await.unwrap();
        assert!(matches!(task, HttpTask::Header(_, false)));
        // The first body bytes came with the header; the rest is not there yet.
        let Some(Ok(HttpTask::Body(Some(body), false))) = http_stream.try_read_response_task()
        else {
            panic!("the buffered body is ready");
        };
        assert_eq!(&body[..], b"abc");
        assert!(http_stream.try_read_response_task().is_none());
        tokio::time::sleep(Duration::from_millis(60)).await;
        let Some(Ok(HttpTask::Body(Some(body), true))) = http_stream.try_read_response_task()
        else {
            panic!("the rest has arrived");
        };
        assert_eq!(&body[..], b"def");
        assert_eq!(http_stream.body_bytes_received(), 6);
        assert!(matches!(
            http_stream.try_read_response_task(),
            Some(Ok(HttpTask::Done))
        ));
    }

    #[tokio::test]
    async fn read_2_buf() {
        init_log();
        let input1 = b"HTTP/1.1 200 OK\r\n";
        let input2 = b"Server: pingora\r\n\r\n";
        let mock_io = Builder::new().read(&input1[..]).read(&input2[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let res = http_stream.read_response().await;
        assert_eq!(input1.len() + input2.len(), res.unwrap());
        assert_eq!(
            input1.len() + input2.len(),
            http_stream.get_headers_raw().len()
        );
        assert_eq!(1, http_stream.resp_header().unwrap().headers.len());
        assert_eq!(http_stream.get_header("Server").unwrap(), "pingora");

        assert_eq!(Some(StatusCode::OK), http_stream.get_status());
        assert_eq!(Version::HTTP_11, http_stream.resp_header().unwrap().version);
    }

    async fn read_response_with_cancellations(
        http_stream: &mut HttpSession,
    ) -> (Result<usize>, usize) {
        let mut cancel_count = 0;

        loop {
            tokio::select! {
                biased;

                _ = tokio::time::sleep(Duration::from_millis(10)) => {
                    cancel_count += 1;
                }
                res = http_stream.read_response() => {
                    return (res, cancel_count);
                }
            }
        }
    }

    async fn read_response_task_with_cancellations(
        http_stream: &mut HttpSession,
    ) -> (Result<HttpTask>, usize) {
        let mut cancel_count = 0;

        loop {
            tokio::select! {
                biased;

                _ = tokio::time::sleep(Duration::from_millis(10)) => {
                    cancel_count += 1;
                }
                res = http_stream.read_response_task() => {
                    return (res, cancel_count);
                }
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn read_response_cancel_safe_after_partial_header_read() {
        init_log();
        let input1 = b"HTTP/1.1 200";
        let input2 = b" OK\r\nContent-Length: 0\r\n\r\n";
        let expected_header = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
        let mock_io = Builder::new()
            .read(&input1[..])
            .wait(Duration::from_millis(100))
            .read(&input2[..])
            .build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));

        let (res, cancel_count) = read_response_with_cancellations(&mut http_stream).await;

        assert!(cancel_count > 0, "read_response should have been cancelled");
        assert_eq!(expected_header.len(), res.unwrap());
        assert_eq!(Some(StatusCode::OK), http_stream.get_status());
        assert_eq!(expected_header, http_stream.get_headers_raw());
    }

    #[tokio::test(start_paused = true)]
    async fn read_response_task_cancel_safe_after_informational_preread() {
        init_log();
        let input1 = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 2";
        let input2 = b"00 OK\r\nContent-Length: 0\r\n\r\n";
        let expected_header = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
        let mock_io = Builder::new()
            .read(&input1[..])
            .wait(Duration::from_millis(100))
            .read(&input2[..])
            .build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));

        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 100);
                assert!(!eob);
            }
            _ => panic!("task should be informational header"),
        }

        let (task, cancel_count) = read_response_task_with_cancellations(&mut http_stream).await;

        assert!(
            cancel_count > 0,
            "final response read should have been cancelled"
        );
        match task.unwrap() {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 200);
                assert!(eob);
            }
            task => panic!("task {task:?} should be final header"),
        }
        assert_eq!(expected_header, http_stream.get_headers_raw());
    }

    #[tokio::test]
    async fn read_response_clears_header_read_state_on_error() {
        init_log();
        let input = b"HTTP/1.1 100 Continue\r\n\r\nHTP/1.1 200 OK\r\n\r\n";
        let mock_io = Builder::new().read(&input[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));

        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 100);
                assert!(!eob);
            }
            _ => panic!("task should be informational header"),
        }

        let err = http_stream.read_response_task().await.unwrap_err();

        assert_eq!(err.etype(), &ErrorType::InvalidHTTPHeader);
        assert!(http_stream.response_header_read_buf.is_empty());
        assert!(http_stream.preread_body.is_none());
    }

    #[tokio::test]
    #[should_panic(expected = "There is still data left to read.")]
    async fn read_invalid() {
        let input1 = b"HTP/1.1 200 OK\r\n";
        let input2 = b"Server: pingora\r\n\r\n";
        let mock_io = Builder::new().read(&input1[..]).read(&input2[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let res = http_stream.read_response().await;
        assert_eq!(&ErrorType::InvalidHTTPHeader, res.unwrap_err().etype());
    }

    #[tokio::test]
    async fn read_rejects_malformed_response_header_name_before_no_case_append() {
        let input = b"HTTP/1.1 200 OK\r\nBad@Name: value\r\nContent-Length: 0\r\n\r\n";
        let mock_io = Builder::new().read(input).build();
        let mut session = HttpSession::new(Box::new(mock_io));

        let error = session.read_response().await.unwrap_err();
        assert_eq!(error.etype(), &ErrorType::InvalidHTTPHeader);
    }

    #[tokio::test]
    async fn read_rejects_invalid_response_header_value_before_unchecked_append() {
        let input = b"HTTP/1.1 200 OK\r\nX-Folded: a\r\n \x7fb\r\n\r\nhello";
        let mock_io = Builder::new().read(input).build();
        let mut session = HttpSession::new(Box::new(mock_io));

        let error = session.read_response().await.unwrap_err();
        assert_eq!(error.etype(), &ErrorType::InvalidHTTPHeader);
    }

    #[tokio::test]
    async fn write() {
        let wire = b"GET /test HTTP/1.1\r\nFoo: Bar\r\n\r\n";
        let mock_io = Builder::new().write(wire).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let mut new_request = RequestHeader::build("GET", b"/test", None).unwrap();
        new_request.insert_header("Foo", "Bar").unwrap();
        let n = http_stream
            .write_request_header(Box::new(new_request))
            .await
            .unwrap();
        assert_eq!(wire.len(), n);
    }

    #[tokio::test]
    async fn borrowed_write_does_not_clone_and_preserves_body_framing() {
        let header = b"POST /test HTTP/1.1\r\nContent-Length: 3\r\n\r\n";
        let body = b"abc";
        let mock_io = Builder::new().write(header).write(body).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let clones = Arc::new(AtomicUsize::new(0));
        let mut request = RequestHeader::build("POST", b"/test", None).unwrap();
        request.insert_header("Content-Length", "3").unwrap();
        request
            .extensions_mut()
            .insert(CloneProbe(Arc::clone(&clones)));

        http_stream
            .write_request_header_ref(&request)
            .await
            .unwrap();
        http_stream.write_body(body).await.unwrap();

        assert_eq!(clones.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn boxed_write_retains_request_extensions_until_session_drop() {
        let wire = b"GET / HTTP/1.1\r\n\r\n";
        let mock_io = Builder::new().write(wire).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let dropped = Arc::new(AtomicBool::new(false));
        let mut request = RequestHeader::build("GET", b"/", None).unwrap();
        request
            .extensions_mut()
            .insert(DropProbe(Arc::clone(&dropped)));

        http_stream
            .write_request_header(Box::new(request))
            .await
            .unwrap();
        assert!(!dropped.load(Ordering::Relaxed));

        drop(http_stream);
        assert!(dropped.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn borrowed_write_leaves_request_extension_lifetime_with_caller() {
        let wire = b"GET / HTTP/1.1\r\n\r\n";
        let mock_io = Builder::new().write(wire).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let dropped = Arc::new(AtomicBool::new(false));
        let mut request = RequestHeader::build("GET", b"/", None).unwrap();
        request
            .extensions_mut()
            .insert(DropProbe(Arc::clone(&dropped)));

        http_stream
            .write_request_header_ref(&request)
            .await
            .unwrap();
        assert!(!dropped.load(Ordering::Relaxed));

        drop(request);
        assert!(dropped.load(Ordering::Relaxed));
        drop(http_stream);
    }

    #[tokio::test]
    async fn borrowed_connection_close_request_disables_reuse() {
        let request_wire = b"GET / HTTP/1.1\r\nConnection: close\r\n\r\n";
        let response = b"HTTP/1.1 200 OK\r\nConnection: keep-alive\r\nContent-Length: 0\r\n\r\n";
        let mock_io = Builder::new().write(request_wire).read(response).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let mut request = RequestHeader::build("GET", b"/", None).unwrap();
        request.insert_header("Connection", "close").unwrap();

        http_stream
            .write_request_header_ref(&request)
            .await
            .unwrap();
        http_stream.read_response().await.unwrap();
        http_stream.respect_keepalive();

        assert!(!http_stream.will_keepalive());
    }

    #[rstest]
    #[case::negative("-1")]
    #[case::not_a_number("abc")]
    #[case::float("1.5")]
    #[case::empty("")]
    #[case::spaces("  ")]
    #[case::mixed("123abc")]
    #[tokio::test]
    async fn validate_response_rejects_invalid_content_length(#[case] invalid_value: &str) {
        init_log();
        let input = format!(
            "HTTP/1.1 200 OK\r\nServer: test\r\nContent-Length: {}\r\n\r\n",
            invalid_value
        );
        let mock_io = Builder::new().read(input.as_bytes()).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        // read_response calls validate_response internally, so it should fail here
        let res = http_stream.read_response().await;
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().etype(), &ErrorType::InvalidHTTPHeader);
    }

    #[tokio::test]
    async fn allow_invalid_content_length_close_delimited_when_configured() {
        init_log();
        let input_header = b"HTTP/1.1 200 OK\r\nServer: test\r\nContent-Length: abc\r\n\r\n";
        let input_body = b"abc";
        let input_close = b"";
        let mock_io = Builder::new()
            .read(&input_header[..])
            .read(&input_body[..])
            .read(&input_close[..])
            .build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let mut peer_options = PeerOptions::new();
        peer_options.allow_h1_response_invalid_content_length = true;
        http_stream.set_allow_h1_response_invalid_content_length(
            peer_options.allow_h1_response_invalid_content_length,
        );

        let res = http_stream.read_response().await;
        assert!(res.is_ok());
        let body = http_stream.read_body_ref().await.unwrap().unwrap();
        assert_eq!(body, input_body);
        assert_eq!(
            http_stream.body_reader.body_state,
            ParseState::UntilClose(3)
        );
        let body = http_stream.read_body_ref().await.unwrap();
        assert!(body.is_none());
        assert_eq!(http_stream.body_reader.body_state, ParseState::Complete(3));
    }

    #[tokio::test]
    async fn allow_invalid_content_length_still_rejects_conflicting_when_configured() {
        // The tolerance option only relaxes a single, otherwise-invalid value.
        // Conflicting/duplicate Content-Length is always an unrecoverable framing
        // error and must be rejected even when the option is enabled.
        init_log();
        let input =
            b"HTTP/1.1 200 OK\r\nServer: test\r\nContent-Length: 3\r\nContent-Length: 4\r\n\r\n";
        let mock_io = Builder::new().read(&input[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        http_stream.set_allow_h1_response_invalid_content_length(true);

        let res = http_stream.read_response().await;
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().etype(), &ErrorType::InvalidHTTPHeader);
    }

    #[rstest]
    #[case::valid_zero("0")]
    #[case::valid_small("123")]
    #[case::valid_large("999999")]
    #[tokio::test]
    async fn validate_response_accepts_valid_content_length(#[case] valid_value: &str) {
        init_log();
        let input = format!(
            "HTTP/1.1 200 OK\r\nServer: test\r\nContent-Length: {}\r\n\r\n",
            valid_value
        );
        let mock_io = Builder::new().read(input.as_bytes()).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let res = http_stream.read_response().await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn response_facts_follow_the_headers_that_were_parsed() {
        for (head, expected) in [
            (
                &b"HTTP/1.1 200 OK\r\nServer: test\r\n\r\n"[..],
                RequestFacts::default(),
            ),
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\n",
                RequestFacts {
                    content_lengths: 1,
                    connection: true,
                    ..RequestFacts::default()
                },
            ),
            (
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 3\r\n\r\n",
                RequestFacts {
                    transfer_encoding: true,
                    ..RequestFacts::default()
                },
            ),
        ] {
            let mock_io = Builder::new().read(head).build();
            let mut http_stream = HttpSession::new(Box::new(mock_io));
            http_stream.read_response().await.unwrap();
            assert_eq!(http_stream.response_noted, expected);
            assert_eq!(
                RequestFacts::of(&http_stream.resp_header().unwrap().headers),
                expected
            );
        }
    }

    #[tokio::test]
    async fn validate_response_accepts_no_content_length() {
        init_log();
        let input = b"HTTP/1.1 200 OK\r\nServer: test\r\n\r\n";
        let mock_io = Builder::new().read(&input[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let res = http_stream.read_response().await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn validate_response_accepts_duplicate_identical_content_length() {
        // RFC 9110 section 8.6 / hyper: identical duplicate Content-Length values
        // are reconciled to a single value and used to frame the body.
        init_log();
        let input_header =
            b"HTTP/1.1 200 OK\r\nServer: test\r\nContent-Length: 3\r\nContent-Length: 3\r\n\r\n";
        let input_body = b"abc";
        let mock_io = Builder::new()
            .read(&input_header[..])
            .read(&input_body[..])
            .build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let res = http_stream.read_response().await;
        assert!(res.is_ok());
        let body = http_stream.read_body_ref().await.unwrap().unwrap();
        assert_eq!(body, input_body);
        assert_eq!(http_stream.body_reader.body_state, ParseState::Complete(3));
        let body = http_stream.read_body_ref().await.unwrap();
        assert!(body.is_none());
    }

    #[tokio::test]
    async fn validate_response_rejects_conflicting_content_length() {
        // RFC 9110 section 8.6 / hyper: differing duplicate Content-Length values
        // are an unrecoverable framing error.
        init_log();
        let input =
            b"HTTP/1.1 200 OK\r\nServer: test\r\nContent-Length: 3\r\nContent-Length: 4\r\n\r\n";
        let mock_io = Builder::new().read(&input[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let res = http_stream.read_response().await;
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().etype(), &ErrorType::InvalidHTTPHeader);
    }

    #[rstest]
    #[case(None, None, None)]
    #[case(Some("transfer-encoding"), None, None)]
    #[case(Some("transfer-encoding"), Some("CONTENT-LENGTH"), Some("4"))]
    #[case(Some("TRANSFER-ENCODING"), Some("CONTENT-LENGTH"), Some("4"))]
    #[case(Some("TRANSFER-ENCODING"), None, None)]
    #[case(None, Some("CONTENT-LENGTH"), Some("4"))]
    #[case(Some("TRANSFER-ENCODING"), Some("content-length"), Some("4"))]
    #[case(None, Some("content-length"), Some("4"))]
    #[case(Some("TRANSFER-ENCODING"), Some("CONTENT-LENGTH"), Some("abc"))]
    #[tokio::test]
    async fn response_transfer_encoding_and_content_length_handling(
        #[case] transfer_encoding_header: Option<&str>,
        #[case] content_length_header: Option<&str>,
        #[case] content_length_value: Option<&str>,
    ) {
        init_log();
        let input1 = b"HTTP/1.1 200 OK\r\n";
        let mut input2 = "Server: test\r\n".to_owned();

        if let Some(transfer_encoding) = transfer_encoding_header {
            input2 += &format!("{transfer_encoding}: chunked\r\n");
        }
        if let Some(content_length) = content_length_header {
            let value = content_length_value.unwrap_or("4");
            input2 += &format!("{content_length}: {value}\r\n")
        }

        input2 += "\r\n";
        let mock_io = Builder::new()
            .read(&input1[..])
            .read(input2.as_bytes())
            .build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let _ = http_stream.read_response().await.unwrap();

        match (content_length_header, transfer_encoding_header) {
            (Some(_) | None, Some(_)) => {
                assert!(http_stream.get_header(header::TRANSFER_ENCODING).is_some());
                assert!(http_stream.get_header(header::CONTENT_LENGTH).is_none());
            }
            (Some(_), None) => {
                assert!(http_stream.get_header(header::TRANSFER_ENCODING).is_none());
                assert!(http_stream.get_header(header::CONTENT_LENGTH).is_some());
            }
            _ => {
                assert!(http_stream.get_header(header::CONTENT_LENGTH).is_none());
                assert!(http_stream.get_header(header::TRANSFER_ENCODING).is_none());
            }
        }
    }

    #[tokio::test]
    #[should_panic(expected = "There is still data left to write.")]
    async fn write_timeout() {
        let wire = b"GET /test HTTP/1.1\r\nFoo: Bar\r\n\r\n";
        let mock_io = Builder::new()
            .wait(Duration::from_secs(2))
            .write(wire)
            .build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        http_stream.write_timeout = Some(Duration::from_secs(1));
        let mut new_request = RequestHeader::build("GET", b"/test", None).unwrap();
        new_request.insert_header("Foo", "Bar").unwrap();
        let res = http_stream
            .write_request_header(Box::new(new_request))
            .await;
        assert_eq!(res.unwrap_err().etype(), &ErrorType::WriteTimedout);
    }

    #[tokio::test]
    #[should_panic(expected = "There is still data left to write.")]
    async fn write_body_timeout() {
        // Test needs Content-Length header to actually attempt to write body
        let header = b"POST /test HTTP/1.1\r\nContent-Length: 3\r\n\r\n";
        let body = b"abc";
        let mock_io = Builder::new()
            .write(&header[..])
            .wait(Duration::from_secs(2))
            .write(&body[..])
            .build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        http_stream.write_timeout = Some(Duration::from_secs(1));

        let mut new_request = RequestHeader::build("POST", b"/test", None).unwrap();
        new_request.insert_header("Content-Length", "3").unwrap();
        http_stream
            .write_request_header(Box::new(new_request))
            .await
            .unwrap();
        let res = http_stream.write_body(body).await;
        assert_eq!(res.unwrap_err().etype(), &WriteTimedout);
    }

    #[tokio::test]
    async fn body_bytes_sent_content_length() {
        let header = b"POST /test HTTP/1.1\r\nContent-Length: 5\r\n\r\n";
        let mock_io = Builder::new()
            .write(&header[..])
            .write(b"ab")
            .write(b"cde")
            .build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));

        let mut new_request = RequestHeader::build("POST", b"/test", None).unwrap();
        new_request.insert_header("Content-Length", "5").unwrap();
        http_stream
            .write_request_header(Box::new(new_request))
            .await
            .unwrap();

        // the request header must not be counted
        assert_eq!(http_stream.body_bytes_sent(), 0);
        http_stream.write_body(b"ab").await.unwrap();
        assert_eq!(http_stream.body_bytes_sent(), 2);
        http_stream.write_body(b"cde").await.unwrap();
        assert_eq!(http_stream.body_bytes_sent(), 5);
    }

    #[tokio::test]
    async fn body_bytes_sent_truncated_write() {
        // Content-Length declares 5 but only 2 bytes are ever written, which is the shape
        // `body_bytes_sent` exists to detect.
        let header = b"POST /test HTTP/1.1\r\nContent-Length: 5\r\n\r\n";
        let mock_io = Builder::new().write(&header[..]).write(b"ab").build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));

        let mut new_request = RequestHeader::build("POST", b"/test", None).unwrap();
        new_request.insert_header("Content-Length", "5").unwrap();
        http_stream
            .write_request_header(Box::new(new_request))
            .await
            .unwrap();
        http_stream.write_body(b"ab").await.unwrap();

        assert_eq!(http_stream.body_bytes_sent(), 2);
    }

    #[cfg(feature = "patched_http1")]
    #[tokio::test]
    async fn write_invalid_path() {
        let wire = b"GET /\x01\xF0\x90\x80 HTTP/1.1\r\nFoo: Bar\r\n\r\n";
        let mock_io = Builder::new().write(wire).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let mut new_request = RequestHeader::build("GET", b"/\x01\xF0\x90\x80", None).unwrap();
        new_request.insert_header("Foo", "Bar").unwrap();
        let n = http_stream
            .write_request_header(Box::new(new_request))
            .await
            .unwrap();
        assert_eq!(wire.len(), n);
    }

    #[rstest]
    #[case(100)]
    #[case(103)]
    #[tokio::test]
    async fn head_informational_response_precedes_final(#[case] informational_status: u16) {
        let request = b"HEAD / HTTP/1.1\r\n\r\n";
        let responses = format!(
            "HTTP/1.1 {informational_status} Informational\r\nX-EaRlY: hint\r\n\r\nHTTP/1.1 200 OK\r\ncOnTeNt-LeNgTh: 7\r\nX-FiNaL: done\r\n\r\n"
        );
        let mock_io = Builder::new()
            .write(&request[..])
            .read(responses.as_bytes())
            .build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let request = RequestHeader::build("HEAD", b"/", None).unwrap();
        http_stream
            .write_request_header_ref(&request)
            .await
            .unwrap();

        match http_stream.read_response_task().await.unwrap() {
            HttpTask::Header(header, end_of_body) => {
                assert_eq!(header.status.as_u16(), informational_status);
                assert!(!end_of_body);
                assert!(!header.has_case());
                let mut wire = Vec::new();
                header.header_to_h1_wire(&mut wire);
                assert_eq!(wire, b"x-early: hint\r\n");
            }
            task => panic!("expected informational header, got {task:?}"),
        }

        match http_stream.read_response_task().await.unwrap() {
            HttpTask::Header(header, end_of_body) => {
                assert_eq!(header.status, StatusCode::OK);
                assert!(end_of_body);
                assert!(!header.has_case());
                let mut wire = Vec::new();
                header.header_to_h1_wire(&mut wire);
                assert_eq!(wire, b"Content-Length: 7\r\nx-final: done\r\n");
            }
            task => panic!("expected final header, got {task:?}"),
        }
        assert!(matches!(
            http_stream.read_response_task().await.unwrap(),
            HttpTask::Done
        ));
    }

    #[tokio::test]
    async fn read_informational() {
        init_log();
        let input1 = b"HTTP/1.1 100 Continue\r\n\r\n";
        let input2 = b"HTTP/1.1 204 OK\r\nServer: pingora\r\n\r\n";
        let mock_io = Builder::new().read(&input1[..]).read(&input2[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));

        // read 100 header first
        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 100);
                assert!(!eob);
            }
            _ => {
                panic!("task should be header")
            }
        }
        // read 200 header next
        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 204);
                assert!(eob);
            }
            _ => {
                panic!("task should be header")
            }
        }
    }

    #[tokio::test]
    async fn read_informational_combined_with_final() {
        init_log();
        let input = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nServer: pingora\r\nContent-Length: 3\r\n\r\n";
        let body = b"abc";
        let mock_io = Builder::new().read(&input[..]).read(&body[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));

        // read 100 header first
        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 100);
                assert!(!eob);
            }
            _ => {
                panic!("task should be header")
            }
        }
        // read 200 header next
        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 200);
                assert!(!eob);
            }
            _ => {
                panic!("task should be header")
            }
        }
        // read body next
        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Body(b, eob) => {
                assert_eq!(b.unwrap(), &body[..]);
                assert!(eob);
            }
            _ => {
                panic!("task {task:?} should be body")
            }
        }
    }

    #[tokio::test]
    async fn read_informational_multiple_combined_with_final() {
        init_log();
        let input = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 103 Early Hints\r\n\r\nHTTP/1.1 204 No Content\r\nServer: pingora\r\n\r\n";
        let mock_io = Builder::new().read(&input[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));

        // read 100 header first
        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 100);
                assert!(!eob);
            }
            _ => {
                panic!("task should be header")
            }
        }

        // then read 103 header
        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 103);
                assert!(!eob);
            }
            _ => {
                panic!("task should be header")
            }
        }

        // finally read 200 header
        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 204);
                assert!(eob);
            }
            _ => {
                panic!("task should be header")
            }
        }
    }

    #[tokio::test]
    async fn read_informational_then_keepalive_response() {
        init_log();
        // Test that after reading an informational response (100 Continue),
        // keepalive still works properly
        let wire = b"GET / HTTP/1.1\r\n\r\n";
        let input1 = b"HTTP/1.1 100 Continue\r\n\r\n";
        let input2 = b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\n\r\n"; // Proper Content-Length
        let body = b"response body";

        let mock_io = Builder::new()
            .write(&wire[..])
            .read(&input1[..])
            .read(&input2[..])
            .read(&body[..])
            .build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));

        // Write request
        let new_request = RequestHeader::build("GET", b"/", None).unwrap();
        http_stream
            .write_request_header(Box::new(new_request))
            .await
            .unwrap();

        // Read 100 Continue
        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 100);
                assert!(!eob);
            }
            _ => {
                panic!("task should be informational header")
            }
        }

        // Read final 200 OK header
        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 200);
                assert!(!eob); // Should not be end of body yet
            }
            _ => {
                panic!("task should be final header")
            }
        }

        // Read body
        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Body(b, eob) => {
                assert_eq!(b.unwrap(), &body[..]);
                assert!(eob); // EOF - body is complete
            }
            _ => {
                panic!("task {task:?} should be body")
            }
        }

        assert_eq!(http_stream.body_reader.body_state, ParseState::Complete(13));

        // Keepalive should be enabled for properly-framed HTTP/1.1
        http_stream.respect_keepalive();
        assert!(http_stream.will_keepalive());
    }

    #[tokio::test]
    async fn init_body_for_upgraded_req() {
        let wire =
            b"GET / HTTP/1.1\r\nConnection: Upgrade\r\nUpgrade: WS\r\nContent-Length: 0\r\n\r\n";
        let input1 = b"HTTP/1.1 101 Switching Protocols\r\ncOnNeCtIoN: Upgrade\r\nuPgRaDe: WS\r\nX-UpGrAdE: retained\r\n\r\n";
        let input2 = b"PAYLOAD";
        let ws_data = b"data";

        let mock_io = Builder::new()
            .write(wire)
            .read(&input1[..])
            .write(&ws_data[..])
            .read(&input2[..])
            .build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let mut new_request = RequestHeader::build("GET", b"/", None).unwrap();
        new_request.insert_header("Connection", "Upgrade").unwrap();
        new_request.insert_header("Upgrade", "WS").unwrap();
        new_request.insert_header("Content-Length", "0").unwrap();
        let _ = http_stream
            .write_request_header_ref(&new_request)
            .await
            .unwrap();
        assert_eq!(
            http_stream.body_writer.body_mode,
            BodyMode::ContentLength(0, 0)
        );
        assert!(http_stream.body_writer.finished());

        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 101);
                assert!(!eob);
                assert!(!h.has_case());
                let mut wire = Vec::new();
                h.header_to_h1_wire(&mut wire);
                assert_eq!(
                    wire,
                    b"Connection: Upgrade\r\nupgrade: WS\r\nx-upgrade: retained\r\n"
                );
            }
            _ => {
                panic!("task should be header")
            }
        }
        // changed body mode
        assert_eq!(
            http_stream.body_reader.body_state,
            ParseState::UntilClose(0)
        );
        // request writer will be explicitly initialized in a separate call
        assert!(http_stream.body_writer.finished());
        http_stream.maybe_upgrade_body_writer();

        assert!(!http_stream.body_writer.finished());
        assert_eq!(http_stream.body_writer.body_mode, BodyMode::UntilClose(0));

        http_stream.write_body(&ws_data[..]).await.unwrap();
        // read WS
        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::UpgradedBody(b, eob) => {
                assert_eq!(b.unwrap(), &input2[..]);
                assert!(!eob);
            }
            _ => {
                panic!("task should be upgraded body")
            }
        }
    }

    #[tokio::test]
    async fn init_preread_body_for_upgraded_req() {
        let wire =
            b"GET / HTTP/1.1\r\nConnection: Upgrade\r\nUpgrade: WS\r\nContent-Length: 0\r\n\r\n";
        let input = b"HTTP/1.1 101 Switching Protocols\r\n\r\nPAYLOAD";
        let ws_data = b"data";

        let mock_io = Builder::new()
            .write(wire)
            .read(&input[..])
            .write(&ws_data[..])
            .build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let mut new_request = RequestHeader::build("GET", b"/", None).unwrap();
        new_request.insert_header("Connection", "Upgrade").unwrap();
        new_request.insert_header("Upgrade", "WS").unwrap();
        new_request.insert_header("Content-Length", "0").unwrap();
        let _ = http_stream
            .write_request_header(Box::new(new_request))
            .await
            .unwrap();
        assert_eq!(
            http_stream.body_writer.body_mode,
            BodyMode::ContentLength(0, 0)
        );
        assert!(http_stream.body_writer.finished());

        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 101);
                assert!(!eob);
            }
            _ => {
                panic!("task should be header")
            }
        }
        // changed body mode
        assert_eq!(
            http_stream.body_reader.body_state,
            ParseState::UntilClose(0)
        );
        // request writer will be explicitly initialized in a separate call
        assert!(http_stream.body_writer.finished());
        http_stream.maybe_upgrade_body_writer();

        assert!(!http_stream.body_writer.finished());
        assert_eq!(http_stream.body_writer.body_mode, BodyMode::UntilClose(0));

        http_stream.write_body(&ws_data[..]).await.unwrap();
        // read WS
        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::UpgradedBody(b, eob) => {
                assert_eq!(b.unwrap(), &b"PAYLOAD"[..]);
                assert!(!eob);
            }
            _ => {
                panic!("task should be upgraded body")
            }
        }
    }

    #[tokio::test]
    async fn read_body_eos_after_upgrade() {
        let wire =
            b"GET / HTTP/1.1\r\nConnection: Upgrade\r\nUpgrade: WS\r\nContent-Length: 10\r\n\r\n";
        let input1 = b"HTTP/1.1 101 Switching Protocols\r\n\r\n";
        let input2 = b"PAYLOAD";
        let body_data = b"0123456789";
        let ws_data = b"data";

        let mock_io = Builder::new()
            .write(wire)
            .read(&input1[..])
            .write(&body_data[..])
            .read(&input2[..])
            .write(&ws_data[..])
            .build();

        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let mut new_request = RequestHeader::build("GET", b"/", None).unwrap();
        new_request.insert_header("Connection", "Upgrade").unwrap();
        new_request.insert_header("Upgrade", "WS").unwrap();
        new_request.insert_header("Content-Length", "10").unwrap();
        let _ = http_stream
            .write_request_header(Box::new(new_request))
            .await
            .unwrap();
        assert_eq!(
            http_stream.body_writer.body_mode,
            BodyMode::ContentLength(10, 0)
        );
        assert!(!http_stream.body_writer.finished());

        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 101);
                assert!(!eob);
            }
            _ => {
                panic!("task should be header")
            }
        }
        // changed body mode
        assert_eq!(
            http_stream.body_reader.body_state,
            ParseState::UntilClose(0)
        );

        // write regular request payload
        http_stream.write_body(&body_data[..]).await.unwrap();
        http_stream.finish_body().await.unwrap();

        // we should still be able to read more response body
        // read WS
        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::UpgradedBody(b, eob) => {
                assert_eq!(b.unwrap(), &input2[..]);
                assert!(!eob);
            }
            t => {
                panic!("task {t:?} should be upgraded body")
            }
        }

        // body IS finished, prior to upgrade on the downstream side
        assert!(http_stream.body_writer.finished());
        http_stream.maybe_upgrade_body_writer();

        assert!(!http_stream.body_writer.finished());
        assert_eq!(http_stream.body_writer.body_mode, BodyMode::UntilClose(0));

        http_stream.write_body(&ws_data[..]).await.unwrap();
        assert_eq!(http_stream.body_writer.body_mode, BodyMode::UntilClose(4));
        http_stream.finish_body().await.unwrap();
    }

    #[tokio::test]
    async fn read_switching_protocol() {
        init_log();

        let wire =
            b"GET / HTTP/1.1\r\nConnection: Upgrade\r\nUpgrade: WS\r\nContent-Length: 0\r\n\r\n";
        let input1 = b"HTTP/1.1 101 Continue\r\n\r\n";
        let input2 = b"PAYLOAD";

        let mock_io = Builder::new()
            .write(&wire[..])
            .read(&input1[..])
            .read(&input2[..])
            .build();

        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let mut new_request = RequestHeader::build("GET", b"/", None).unwrap();
        new_request.insert_header("Connection", "Upgrade").unwrap();
        new_request.insert_header("Upgrade", "WS").unwrap();
        new_request.insert_header("Content-Length", "0").unwrap();
        let _ = http_stream
            .write_request_header(Box::new(new_request))
            .await
            .unwrap();
        assert_eq!(
            http_stream.body_writer.body_mode,
            BodyMode::ContentLength(0, 0)
        );
        assert!(http_stream.body_writer.finished());

        // read 100 header first
        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 101);
                assert!(!eob);
            }
            _ => {
                panic!("task should be header")
            }
        }
        // read body
        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::UpgradedBody(b, eob) => {
                assert_eq!(b.unwrap(), &input2[..]);
                assert!(!eob);
            }
            _ => {
                panic!("task should be upgraded body")
            }
        }
        // read body
        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::UpgradedBody(b, eob) => {
                assert!(b.is_none());
                assert!(eob);
            }
            _ => {
                panic!("task should be body with end of stream")
            }
        }
    }

    // Each obs-fold (CRLF + at least one SP/HTAB) in a received header
    // value is replaced with a single SP before the value is interpreted,
    // via `pingora_http::header_value_from_raw`.
    #[tokio::test]
    async fn read_obsolete_multiline_headers() {
        init_log();
        let input = b"HTTP/1.1 200 OK\r\nServer : pingora\r\n Foo: Bar\r\n\r\n";
        let mock_io = Builder::new().read(&input[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let res = http_stream.read_response().await;
        assert_eq!(input.len(), res.unwrap());

        assert_eq!(1, http_stream.resp_header().unwrap().headers.len());
        assert_eq!(
            http_stream.get_header("Server").unwrap(),
            "pingora Foo: Bar"
        );

        let input = b"HTTP/1.1 200 OK\r\nServer : pingora\r\n\t  Fizz: Buzz\r\n\r\n";
        let mock_io = Builder::new().read(&input[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let res = http_stream.read_response().await;
        assert_eq!(input.len(), res.unwrap());
        assert_eq!(1, http_stream.resp_header().unwrap().headers.len());
        assert_eq!(
            http_stream.get_header("Server").unwrap(),
            "pingora Fizz: Buzz"
        );
    }

    #[cfg(feature = "patched_http1")]
    #[tokio::test]
    async fn read_headers_skip_invalid_line() {
        init_log();
        let input = b"HTTP/1.1 200 OK\r\n;\r\nFoo: Bar\r\n\r\n";
        let mock_io = Builder::new().read(&input[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        let res = http_stream.read_response().await;
        assert_eq!(input.len(), res.unwrap());
        assert_eq!(1, http_stream.resp_header().unwrap().headers.len());
        assert_eq!(http_stream.get_header("Foo").unwrap(), "Bar");
    }

    #[tokio::test]
    async fn read_keepalive_headers() {
        init_log();

        async fn build_resp_with_keepalive(conn: &str) -> HttpSession {
            // Include Content-Length to avoid triggering defense-in-depth close-delimited check
            let input =
                format!("HTTP/1.1 200 OK\r\nConnection: {conn}\r\nContent-Length: 0\r\n\r\n");
            let mock_io = Builder::new().read(input.as_bytes()).build();
            let mut http_stream = HttpSession::new(Box::new(mock_io));
            let res = http_stream.read_response().await;
            assert_eq!(input.len(), res.unwrap());
            http_stream.respect_keepalive();
            http_stream
        }

        assert_eq!(
            build_resp_with_keepalive("close").await.keepalive_timeout,
            KeepaliveStatus::Off
        );

        assert_eq!(
            build_resp_with_keepalive("keep-alive")
                .await
                .keepalive_timeout,
            KeepaliveStatus::Infinite
        );

        assert_eq!(
            build_resp_with_keepalive("foo").await.keepalive_timeout,
            KeepaliveStatus::Infinite
        );

        assert_eq!(
            build_resp_with_keepalive("upgrade,close")
                .await
                .keepalive_timeout,
            KeepaliveStatus::Off
        );

        assert_eq!(
            build_resp_with_keepalive("upgrade, close")
                .await
                .keepalive_timeout,
            KeepaliveStatus::Off
        );

        assert_eq!(
            build_resp_with_keepalive("Upgrade, close")
                .await
                .keepalive_timeout,
            KeepaliveStatus::Off
        );

        assert_eq!(
            build_resp_with_keepalive("Upgrade,close")
                .await
                .keepalive_timeout,
            KeepaliveStatus::Off
        );

        assert_eq!(
            build_resp_with_keepalive("close,upgrade")
                .await
                .keepalive_timeout,
            KeepaliveStatus::Off
        );

        assert_eq!(
            build_resp_with_keepalive("close, upgrade")
                .await
                .keepalive_timeout,
            KeepaliveStatus::Off
        );

        assert_eq!(
            build_resp_with_keepalive("close,Upgrade")
                .await
                .keepalive_timeout,
            KeepaliveStatus::Off
        );

        assert_eq!(
            build_resp_with_keepalive("close, Upgrade")
                .await
                .keepalive_timeout,
            KeepaliveStatus::Off
        );

        async fn build_resp_with_keepalive_values(keep_alive: &str) -> HttpSession {
            let input = format!("HTTP/1.1 200 OK\r\nKeep-Alive: {keep_alive}\r\n\r\n");
            let mock_io = Builder::new().read(input.as_bytes()).build();
            let mut http_stream = HttpSession::new(Box::new(mock_io));
            let res = http_stream.read_response().await;
            assert_eq!(input.len(), res.unwrap());
            http_stream.respect_keepalive();
            http_stream
        }

        assert_eq!(
            build_resp_with_keepalive_values("timeout=5, max=1000")
                .await
                .get_keepalive_values(),
            (Some(5), Some(1000))
        );

        assert_eq!(
            build_resp_with_keepalive_values("max=1000, timeout=5")
                .await
                .get_keepalive_values(),
            (Some(5), Some(1000))
        );

        assert_eq!(
            build_resp_with_keepalive_values(" timeout = 5, max = 1000 ")
                .await
                .get_keepalive_values(),
            (Some(5), Some(1000))
        );

        assert_eq!(
            build_resp_with_keepalive_values("timeout=5")
                .await
                .get_keepalive_values(),
            (Some(5), None)
        );

        assert_eq!(
            build_resp_with_keepalive_values("max=1000")
                .await
                .get_keepalive_values(),
            (None, Some(1000))
        );

        assert_eq!(
            build_resp_with_keepalive_values("a=b")
                .await
                .get_keepalive_values(),
            (None, None)
        );

        assert_eq!(
            build_resp_with_keepalive_values("")
                .await
                .get_keepalive_values(),
            (None, None)
        );
    }

    /* Note: body tests are covered in server.rs */

    #[tokio::test]
    async fn test_http10_response_with_transfer_encoding_disables_keepalive() {
        // Transfer-Encoding in HTTP/1.0 response requires connection close
        let input = b"HTTP/1.0 200 OK\r\n\
Transfer-Encoding: chunked\r\n\
Connection: keep-alive\r\n\
\r\n\
5\r\n\
hello\r\n\
0\r\n\
\r\n";
        let mock_io = Builder::new().read(&input[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        http_stream.read_response().await.unwrap();
        http_stream.respect_keepalive();

        // Keepalive must be disabled even if Connection: keep-alive header present
        assert!(!http_stream.will_keepalive());
        assert_eq!(http_stream.keepalive_timeout, KeepaliveStatus::Off);
    }

    #[tokio::test]
    async fn test_http11_response_with_transfer_encoding_allows_keepalive() {
        // HTTP/1.1 with Transfer-Encoding should allow keepalive (contrast with HTTP/1.0)
        let input = b"HTTP/1.1 200 OK\r\n\
Transfer-Encoding: chunked\r\n\
\r\n\
5\r\n\
hello\r\n\
0\r\n\
\r\n";
        let mock_io = Builder::new().read(&input[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        http_stream.read_response().await.unwrap();
        http_stream.respect_keepalive();

        // HTTP/1.1 should allow keepalive by default
        assert!(http_stream.will_keepalive());
    }

    #[tokio::test]
    async fn test_malformed_chunked_response_is_not_reusable() {
        let input = b"HTTP/1.1 503 Service Unavailable\r\nTransfer-Encoding: chunked\r\n\r\nZZ\r\n";
        let mock_io = Builder::new().read(&input[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));

        let task = http_stream.read_response_task().await.unwrap();
        assert!(matches!(task, HttpTask::Header(_, false)));
        assert!(http_stream.read_response_task().await.is_err());
        assert!(http_stream.is_body_done());
        assert!(!http_stream.body_reader.body_complete());

        http_stream.respect_keepalive();
        assert!(http_stream.will_keepalive());
        assert!(http_stream.reuse().await.is_none());
    }

    #[tokio::test]
    async fn test_reuse_rechecks_overread_after_early_keepalive() {
        let input =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\na\r\n0\r\n\r\nextra";
        let mock_io = Builder::new().read(&input[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));

        http_stream.read_response().await.unwrap();
        http_stream.respect_keepalive();
        assert!(http_stream.will_keepalive());

        while http_stream.read_body_bytes().await.unwrap().is_some() {}
        assert!(http_stream.body_reader.body_complete());
        assert!(http_stream.body_reader.has_bytes_overread());
        assert!(http_stream.reuse().await.is_none());
    }

    #[tokio::test]
    async fn test_response_multiple_transfer_encoding_headers() {
        init_log();
        // Multiple TE headers should be treated as comma-separated
        let input = b"HTTP/1.1 200 OK\r\n\
Transfer-Encoding: gzip\r\n\
Transfer-Encoding: chunked\r\n\
\r\n\
5\r\n\
hello\r\n\
0\r\n\
\r\n";

        let mock_io = Builder::new().read(&input[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        http_stream.read_response().await.unwrap();

        // Should correctly identify chunked encoding from last header
        assert!(http_stream.is_chunked_encoding());

        // Verify body can be read correctly
        let body = http_stream.read_body_bytes().await.unwrap();
        assert_eq!(body.as_ref().unwrap().as_ref(), b"hello");
        http_stream.finish_body().await.unwrap();
    }

    #[tokio::test]
    async fn test_response_multiple_te_headers_chunked_not_last() {
        init_log();
        // Chunked in first header but not last - should NOT be chunked
        let input = b"HTTP/1.1 200 OK\r\n\
Transfer-Encoding: chunked\r\n\
Transfer-Encoding: identity\r\n\
Content-Length: 5\r\n\
\r\n\
hello";

        let mock_io = Builder::new().read(&input[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));
        http_stream.read_response().await.unwrap();

        // Should NOT be chunked - identity is final encoding
        assert!(!http_stream.is_chunked_encoding());
    }

    #[test]
    fn test_is_chunked_encoding_before_response() {
        // Test that is_chunked_encoding returns false when no response received yet
        let mock_io = Builder::new().build();
        let http_stream = HttpSession::new(Box::new(mock_io));

        // Should return false when no response header exists yet
        assert!(!http_stream.is_chunked_encoding());
    }

    #[tokio::test]
    async fn write_request_body_implicit_zero_content_length() {
        init_log();
        let header = b"POST /test HTTP/1.1\r\n\r\n";
        let mock_io = Builder::new().write(&header[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));

        let new_request = RequestHeader::build("POST", b"/test", None).unwrap();
        http_stream
            .write_request_header(Box::new(new_request))
            .await
            .unwrap();

        assert_eq!(
            http_stream.body_writer.body_mode,
            BodyMode::ContentLength(0, 0)
        );
    }

    #[tokio::test]
    async fn write_request_body_with_content_length() {
        init_log();
        let header = b"POST /test HTTP/1.1\r\nContent-Length: 3\r\n\r\n";
        let body = b"abc";
        let mock_io = Builder::new().write(&header[..]).write(&body[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));

        let mut new_request = RequestHeader::build("POST", b"/test", None).unwrap();
        new_request.insert_header("Content-Length", "3").unwrap();
        http_stream
            .write_request_header(Box::new(new_request))
            .await
            .unwrap();

        assert_eq!(
            http_stream.body_writer.body_mode,
            BodyMode::ContentLength(3, 0)
        );

        http_stream.write_body(body).await.unwrap();
        assert_eq!(
            http_stream.body_writer.body_mode,
            BodyMode::ContentLength(3, 3)
        );
    }

    #[tokio::test]
    async fn close_delimited_response_explicitly_disables_keepalive() {
        init_log();
        // Defense-in-depth: if we read a close-delimited response body,
        // keepalive should be disabled
        let wire = b"GET / HTTP/1.1\r\n\r\n";
        let input_header = b"HTTP/1.1 200 OK\r\n\r\n";
        let input_body = b"abc";
        let input_close = b""; // simulating close
        let mock_io = Builder::new()
            .write(&wire[..])
            .read(&input_header[..])
            .read(&input_body[..])
            .read(&input_close[..])
            .build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));

        // Write request first
        let new_request = RequestHeader::build("GET", b"/", None).unwrap();
        http_stream
            .write_request_header(Box::new(new_request))
            .await
            .unwrap();

        // Read response
        http_stream.read_response().await.unwrap();

        // Read the body (this will initialize the body reader)
        http_stream.read_body_ref().await.unwrap();

        // Body reader should be in UntilClose mode (close-delimited response)
        assert_eq!(
            http_stream.body_reader.body_state,
            ParseState::UntilClose(3)
        );

        let res2 = http_stream.read_body_ref().await.unwrap();
        assert!(res2.is_none()); // EOF

        // Body should now be Complete
        assert_eq!(http_stream.body_reader.body_state, ParseState::Complete(3));

        http_stream.respect_keepalive();
        assert!(!http_stream.will_keepalive());
    }

    #[tokio::test]
    async fn read_informational_head_request() {
        init_log();
        // HEAD request that receives 100 Continue followed by 200 OK
        let wire = b"HEAD / HTTP/1.1\r\n\r\n";
        let input1 = b"HTTP/1.1 100 Continue\r\n\r\n";
        let input2 = b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n";

        let mock_io = Builder::new()
            .write(&wire[..])
            .read(&input1[..])
            .read(&input2[..])
            .build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));

        // Write HEAD request
        let new_request = RequestHeader::build("HEAD", b"/", None).unwrap();
        http_stream
            .write_request_header(Box::new(new_request))
            .await
            .unwrap();

        // Read 100 Continue
        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 100);
                assert!(!eob, "100 Continue for HEAD should not signal end of body");
            }
            _ => {
                panic!("task should be informational header")
            }
        }

        // Read final 200 OK
        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 200);
                assert!(eob, "HEAD 200 response should signal end of body");
            }
            _ => {
                panic!("task should be final header")
            }
        }

        // Body reader should be Complete(0) for HEAD
        assert_eq!(http_stream.body_reader.body_state, ParseState::Complete(0));
    }

    #[tokio::test]
    async fn read_informational_multiple_head_request() {
        init_log();
        // HEAD request that receives 100 Continue, 103 Early Hints, then 200 OK
        let wire = b"HEAD / HTTP/1.1\r\n\r\n";
        let input = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 103 Early Hints\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 50\r\n\r\n";

        let mock_io = Builder::new().write(&wire[..]).read(&input[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));

        let new_request = RequestHeader::build("HEAD", b"/", None).unwrap();
        http_stream
            .write_request_header(Box::new(new_request))
            .await
            .unwrap();

        // Read 100 Continue
        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 100);
                assert!(!eob, "100 Continue for HEAD should not signal end of body");
            }
            _ => {
                panic!("task should be 100 header")
            }
        }

        // Read 103 Early Hints
        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 103);
                assert!(
                    !eob,
                    "103 Early Hints for HEAD should not signal end of body"
                );
            }
            _ => {
                panic!("task should be 103 header")
            }
        }

        // Read 200 OK — end of body
        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 200);
                assert!(eob, "HEAD 200 response should signal end of body");
            }
            _ => {
                panic!("task should be final header")
            }
        }

        assert_eq!(http_stream.body_reader.body_state, ParseState::Complete(0));
    }

    #[tokio::test]
    async fn read_basic_head() {
        init_log();
        // Basic HEAD + 200
        let wire = b"HEAD / HTTP/1.1\r\n\r\n";
        let input = b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n";

        let mock_io = Builder::new().write(&wire[..]).read(&input[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));

        let new_request = RequestHeader::build("HEAD", b"/", None).unwrap();
        http_stream
            .write_request_header(Box::new(new_request))
            .await
            .unwrap();

        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 200);
                assert!(eob, "HEAD 200 should be end of body");
            }
            _ => {
                panic!("task should be header")
            }
        }

        assert_eq!(http_stream.body_reader.body_state, ParseState::Complete(0));

        // Keepalive should work for a properly-framed HEAD response
        http_stream.respect_keepalive();
        assert!(http_stream.will_keepalive());
    }

    #[tokio::test]
    async fn read_head_informational_keepalive() {
        init_log();
        // HEAD + 100 Continue + 200 OK, then verify keepalive is preserved.
        let wire = b"HEAD / HTTP/1.1\r\n\r\n";
        let input1 = b"HTTP/1.1 100 Continue\r\n\r\n";
        let input2 = b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n";

        let mock_io = Builder::new()
            .write(&wire[..])
            .read(&input1[..])
            .read(&input2[..])
            .build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));

        let new_request = RequestHeader::build("HEAD", b"/", None).unwrap();
        http_stream
            .write_request_header(Box::new(new_request))
            .await
            .unwrap();

        // 100 Continue
        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 100);
                assert!(!eob);
            }
            _ => panic!("task should be informational header"),
        }

        // 200 OK
        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 200);
                assert!(eob);
            }
            _ => panic!("task should be final header"),
        }

        assert_eq!(http_stream.body_reader.body_state, ParseState::Complete(0));

        // Keepalive must still work after the 100 + 200 sequence
        http_stream.respect_keepalive();
        assert!(http_stream.will_keepalive());
    }

    #[tokio::test]
    async fn read_head_204() {
        init_log();
        // HEAD + 204 No Content
        let wire = b"HEAD / HTTP/1.1\r\n\r\n";
        let input = b"HTTP/1.1 204 No Content\r\n\r\n";

        let mock_io = Builder::new().write(&wire[..]).read(&input[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));

        let new_request = RequestHeader::build("HEAD", b"/", None).unwrap();
        http_stream
            .write_request_header(Box::new(new_request))
            .await
            .unwrap();

        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 204);
                assert!(eob, "HEAD 204 should be end of body");
            }
            _ => panic!("task should be header"),
        }

        assert_eq!(http_stream.body_reader.body_state, ParseState::Complete(0));
    }

    #[tokio::test]
    async fn read_head_304() {
        init_log();
        // HEAD + 304 Not Modified
        let wire = b"HEAD / HTTP/1.1\r\n\r\n";
        let input = b"HTTP/1.1 304 Not Modified\r\nContent-Length: 100\r\n\r\n";

        let mock_io = Builder::new().write(&wire[..]).read(&input[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));

        let new_request = RequestHeader::build("HEAD", b"/", None).unwrap();
        http_stream
            .write_request_header(Box::new(new_request))
            .await
            .unwrap();

        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 304);
                assert!(eob, "HEAD 304 should be end of body");
            }
            _ => panic!("task should be header"),
        }

        assert_eq!(http_stream.body_reader.body_state, ParseState::Complete(0));
    }

    #[tokio::test]
    async fn read_head_101_non_upgrade() {
        init_log();
        // HEAD + 101 where the request is not an upgrade request.
        // Contrived, but verifies the new code path: 101 check fires first,
        // is_upgrade_req() returns false, then HEAD check fires.
        let wire = b"HEAD / HTTP/1.1\r\n\r\n";
        let input = b"HTTP/1.1 101 Switching Protocols\r\n\r\n";

        let mock_io = Builder::new().write(&wire[..]).read(&input[..]).build();
        let mut http_stream = HttpSession::new(Box::new(mock_io));

        let new_request = RequestHeader::build("HEAD", b"/", None).unwrap();
        http_stream
            .write_request_header(Box::new(new_request))
            .await
            .unwrap();

        let task = http_stream.read_response_task().await.unwrap();
        match task {
            HttpTask::Header(h, eob) => {
                assert_eq!(h.status, 101);
                // HEAD without Upgrade headers → not an upgrade, body is "done"
                assert!(eob, "HEAD 101 (non-upgrade) should be end of body");
            }
            _ => panic!("task should be header"),
        }

        assert_eq!(http_stream.body_reader.body_state, ParseState::Complete(0));
    }
}

#[cfg(test)]
mod test_sync {
    use super::*;
    use log::error;

    #[test]
    fn test_request_to_wire() {
        let mut new_request = RequestHeader::build("GET", b"/", None).unwrap();
        new_request.insert_header("Foo", "Bar").unwrap();
        let wire = http_req_header_to_wire(&new_request).unwrap();
        let mut headers = [httparse::EMPTY_HEADER; 128];
        let mut req = httparse::Request::new(&mut headers);
        let result = req.parse(wire.as_ref());
        match result {
            Ok(_) => {}
            Err(e) => error!("{:?}", e),
        }
        assert!(result.unwrap().is_complete());
        // FIXME: the order is not guaranteed
        assert_eq!("/", req.path.unwrap());
        assert_eq!(b"Foo", headers[0].name.as_bytes());
        assert_eq!(b"Bar", headers[0].value);
    }

    #[test]
    fn test_absolute_form_and_connect_to_wire() {
        // The request-line written to the upstream is built from raw_path(), so both
        // the absolute-form target (RFC 9112 §3.2.2) and the CONNECT authority-form
        // (§3.2.3) are sent verbatim. Choosing origin-form instead is a decision for
        // the proxy layer, which knows whether the next hop is an origin or a proxy.
        let request_line = |method: &str, target: &[u8]| -> String {
            let req = RequestHeader::build(method, target, None).unwrap();
            let wire = http_req_header_to_wire(&req).unwrap();
            let line = wire.as_ref().split(|&b| b == b'\r').next().unwrap();
            String::from_utf8(line.to_vec()).unwrap()
        };

        // §3.2.2 example request-target.
        assert_eq!(
            "GET http://www.example.org/pub/WWW/TheProject.html HTTP/1.1",
            request_line("GET", b"http://www.example.org/pub/WWW/TheProject.html")
        );
        assert_eq!(
            "GET http://host?q=1 HTTP/1.1",
            request_line("GET", b"http://host?q=1")
        );
        assert_eq!(
            "GET http://host HTTP/1.1",
            request_line("GET", b"http://host")
        );
        // §3.2.3 example CONNECT request-target.
        assert_eq!(
            "CONNECT www.example.com:80 HTTP/1.1",
            request_line("CONNECT", b"www.example.com:80")
        );
        // Origin-form is unaffected.
        assert_eq!("GET /a?q=1 HTTP/1.1", request_line("GET", b"/a?q=1"));

        // A fragment is not part of the request-target (§3.2) and never reaches the
        // wire, so it cannot be used to desync what a cache or a filter in front of us
        // sees from what the upstream receives.
        assert_eq!(
            "GET http://host/a HTTP/1.1",
            request_line("GET", b"http://host/a#frag")
        );
        assert_eq!(
            "GET http://host HTTP/1.1",
            request_line("GET", b"http://host#@evil.example/")
        );
        assert_eq!(
            "CONNECT www.example.com:80 HTTP/1.1",
            request_line("CONNECT", b"www.example.com:80#x")
        );
        assert_eq!("GET /a HTTP/1.1", request_line("GET", b"/a#frag"));

        // Stripping a fragment can leave nothing behind. An empty request-target would
        // serialize as "GET  HTTP/1.1", which is malformed and which our own H1 parser
        // rejects, so it has to fall back to the root instead.
        assert_eq!("GET / HTTP/1.1", request_line("GET", b"#frag"));
        assert_eq!("GET / HTTP/1.1", request_line("GET", b""));
        // Asterisk-form and query-only targets keep their form through the strip.
        assert_eq!("OPTIONS * HTTP/1.1", request_line("OPTIONS", b"*#frag"));
        assert_eq!("GET ?q=1 HTTP/1.1", request_line("GET", b"?q=1#frag"));
    }

    /// Deterministic, parser-independent test of the request-line delimiter
    /// guard. Testing it through `http_req_header_to_wire`/`RequestHeader` is
    /// unreliable because whether `set_raw_path` admits a delimiter byte depends
    /// on the linked `http` crate's URI validation; the predicate itself has no
    /// such dependency.
    #[test]
    fn test_request_target_has_forbidden_byte() {
        const FORBIDDEN: [u8; 4] = [0x00, 0x0a, 0x0d, 0x20];
        for b in 0x00u8..=0xff {
            let expected = FORBIDDEN.contains(&b);
            assert_eq!(
                request_target_has_forbidden_byte(&[b]),
                expected,
                "byte {b:#04x}: expected forbidden={expected}",
            );
        }
        assert!(!request_target_has_forbidden_byte(b"/normal/path?a=b"));
        assert!(request_target_has_forbidden_byte(b"/x HTTP/1.1")); // SP
        assert!(request_target_has_forbidden_byte(b"/a\r\nb")); // CR/LF
    }
}
