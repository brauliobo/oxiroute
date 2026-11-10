//! HTTP/1 head codec of the compiled fast lane.
//!
//! The lane sits in front of Pingora's HTTP/1 session and may handle a request only when its reading of the bytes is
//! provably the one the classic path has. This crate is the part that decides and that writes: it scans request and
//! response heads without allocating, and writes them back byte for byte as the classic path would. The classic path is
//! the oracle; `tests/` replays its vendored sequence (httparse, `RequestHeader::build_no_case`, one `append` per
//! header, the `HeaderMap` edits, `header_to_h1_wire`, `http_resp_header_to_buf`) and compares.
//!
//! No `std`, no `tokio`, no `unsafe`; `alloc` only for the `Vec` the writers append to.
//!
//! # Scanning a request
//!
//! [`scan_request`] reads the head at the start of a buffer into a reusable [`RequestHead`] (offsets into the buffer,
//! at most [`MAX_HEADERS`] headers, [`RequestFacts`]) and answers [`Scan`]:
//!
//! * `Partial`: more bytes could still make a head. Every proper prefix of an accepted head is `Partial`.
//! * `Miss(reason)`: not the lane's. The classic path reads the same bytes from the start; nothing was consumed.
//! * `Head`: the classic parser accepts the same bytes and reads the same method, target, version, header list (names
//!   as sent, values trimmed of the whitespace around them, arrival order) and facts. Bytes after
//!   [`RequestHead::head_len`] are neither consumed nor judged, so the caller sees pipelining by comparing it with the
//!   buffer length; appending bytes after the head never changes the result.
//!
//! A miss is a refusal of a superset of what the classic parser refuses, plus every shape that is read differently or
//! that a lane has no business interpreting:
//!
//! * bare LF or bare CR anywhere, a space or tab at the start of a header line (obsolete folding), whitespace before the
//!   colon, an empty or non-token name, a control byte other than a tab or DEL in a value, a name over
//!   [`MAX_NAME_LEN`] bytes, more than [`MAX_HEADERS`] headers;
//! * an empty line before the request line, a method that is not a token of at most [`MAX_METHOD_LEN`] bytes followed
//!   by one space (any token is accepted, the caller filters), a request line that classic would rewrite through
//!   `escape_illegal_request_line`;
//! * a target that is not origin-form (`/...`), over [`MAX_TARGET_LEN`] bytes, or with a byte `http::Uri` would store
//!   differently (high bytes, `#`, `"`, backtick, `<`, `>`, space, controls): accepted targets are written back as sent;
//! * a version other than `HTTP/1.1` (`HTTP/1.0` included);
//! * a missing or repeated `Host`;
//! * a `Content-Length` that is repeated or not 1 to 18 digits (signs, commas, hex and overflow included);
//! * a `Transfer-Encoding` whose last header does not end in `chunked` (classic rejects it).
//!
//! The facts are noted in the same pass: `Host` index, `Content-Length` (none when a `Transfer-Encoding` is present, as
//! classic drops it), `Transfer-Encoding` / `Upgrade` / `Expect` presence, [`Connection`] (`Other` for any value but
//! `keep-alive` or `close`, or more than one header), the six fixed hop-by-hop fields as a bitmask
//! ([`HOP_KEEP_ALIVE`] and the rest), `X-Request-Id` count and first index, underscore and capital letters in names, and
//! whether two names repeat. Names are classified by length and first byte into a [`Class`]; [`Header::flags`] says
//! whether a line is already byte for byte what the classic writers emit.
//!
//! # Scanning a response
//!
//! [`scan_response`] answers [`ScanResponse`]: `Partial`, `Head`, or `Slow`, which is not an error: the head is not the
//! common clean shape (`HTTP/1.0` or `HTTP/1.1`, a 3-digit status of 100 to 599, a reason of tabs, spaces and visible
//! ASCII, CRLF only, the header strictness of requests, at most [`MAX_HEADERS`] headers, a single digits-only
//! `Content-Length`, `Connection` absent, `keep-alive` or `close`) or the scanner is unsure, so the classic parser
//! decides and may well accept it. `Head` implies the classic client parser reads the same status, version, reason,
//! header list and facts ([`ResponseFacts`]: framing, `chunked` last, hop-by-hop bitmask including `Connection`,
//! `Transfer-Encoding` and `Upgrade`, request-id presence, informational).
//!
//! # Writing
//!
//! [`write_request`] and [`write_response`] append the head to a `Vec<u8>`, applying [`Edits`] in order. The classic
//! path keeps headers in an `http::HeaderMap`, whose iteration order is not the order of arrival, and writes names in
//! title case for twelve standard names and lowercase for the rest. The writers reproduce both, without the map:
//!
//! * entries are ordered by the first occurrence of a name (case-insensitive); the further values of a name are
//!   grouped right after its entry, in arrival order;
//! * [`Edit::Remove`] (`HeaderMap::remove`) drops all values of a name and moves the last entry into the hole;
//! * [`Edit::Set`] (`HeaderMap::insert`) replaces the first value in place, drops the others, or appends an entry;
//! * [`Edit::Add`] (`HeaderMap::append`) adds a value after the last of its name, or an entry at the end;
//! * names are written title-cased for `Accept-Ranges`, `Age`, `Cache-Control`, `Connection`, `Content-Type`,
//!   `Content-Encoding`, `Content-Length`, `Date`, `Transfer-Encoding`, `Host`, `Server` and `Set-Cookie`, lowercase
//!   otherwise; values are copied verbatim (they were trimmed by the scan).
//!
//! [`HeaderOrder`] is that emulation on its own. The response writer takes the version of the status line and, for a
//! final response, an [`Automatic`] date and connection word: any `Date` and `Connection` entries are left out wherever
//! they are and written last, as `http_resp_header_to_buf` does. [`Edits::stamp_request_id`] expresses
//! `stamp_response` (an upstream id moves to `X-Upstream-Request-Id` when it differs). When a head has a
//! `Transfer-Encoding` the classic path has already removed its `Content-Length`: the lane adds that
//! [`names::CONTENT_LENGTH`] removal first.
//!
//! ```
//! use oxiroute_h1_head::{Edits, RequestHead, Scan, Value, names, scan_request, write_request};
//!
//! let buf = b"GET /a?b=1 HTTP/1.1\r\nHost: example.com\r\nAccept:*/*\r\nConnection: close\r\n\r\n";
//! let mut head = RequestHead::new();
//! assert_eq!(scan_request(buf, &mut head), Scan::Head);
//! assert_eq!(head.host(buf), b"example.com");
//!
//! let mut edits = Edits::new();
//! edits.remove(names::CONNECTION).unwrap();
//! edits.set(names::X_REQUEST_ID, Value::new(b"0123456789abcdef").unwrap()).unwrap();
//! let mut out = Vec::new();
//! write_request(&mut out, buf, &head, &edits);
//! assert_eq!(out, b"GET /a?b=1 HTTP/1.1\r\nHost: example.com\r\naccept: */*\r\nx-request-id: 0123456789abcdef\r\n\r\n");
//! ```

#![no_std]

extern crate alloc;

mod class;
mod edit;
mod emit;
mod fields;
mod miss;
mod order;
mod request;
mod response;
mod tables;
mod write_request;
mod write_response;

pub use class::{Class, classify};
pub use class::{
    HOP_BY_HOP_REQUEST, HOP_BY_HOP_RESPONSE, HOP_CONNECTION, HOP_KEEP_ALIVE, HOP_PROXY_AUTHENTICATE,
};
pub use class::{
    HOP_PROXY_AUTHORIZATION, HOP_PROXY_CONNECTION, HOP_TE, HOP_TRAILER, HOP_TRANSFER_ENCODING,
    HOP_UPGRADE,
};
pub use edit::{Edit, Edits, EditsFull, MAX_EDITS, Name, Value, names};
pub use fields::{
    Header, LINE_IS_CLEAN, MAX_HEADERS, MAX_NAME_LEN, NAME_HAS_UNDERSCORE, NAME_HAS_UPPER,
};
pub use miss::{MissReason, Scan, ScanResponse};
pub use order::{HeaderOrder, Items, MAX_ITEMS};
pub use request::*;
pub use response::{ResponseFacts, ResponseHead, scan_response};
pub use tables::is_header_value;
pub use write_request::write_request;
pub use write_response::{Automatic, write_response};
