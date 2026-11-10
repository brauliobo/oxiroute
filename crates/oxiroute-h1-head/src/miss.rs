//! Why a request head is not the lane's.

/// The reason a request head was handed to the classic path. Every reason is a decision about the bytes seen so far:
/// a head that is a prefix of an accepted head never gets one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MissReason {
    /// The buffer is longer than a `u32` offset can address.
    TooLarge,
    /// An empty line before the request line, or a method that is not a token followed by one space.
    Method,
    MethodTooLong,
    /// The target is not origin-form (`/...`) or has a byte `http::Uri` would store differently.
    Target,
    TargetTooLong,
    /// Not exactly `HTTP/1.1` followed by CRLF.
    Version,
    /// `HTTP/1.0`.
    Version10,
    BareLf,
    BareCr,
    /// A header line starting with a space or a tab.
    Folding,
    /// A space or a tab between the name and the colon.
    SpaceBeforeColon,
    /// A control byte other than a tab, or DEL, in a value.
    Control,
    /// An empty name or a byte outside the token set before the colon.
    Name,
    NameTooLong,
    TooManyHeaders,
    HostMissing,
    HostDuplicate,
    /// A `Transfer-Encoding` whose last header does not end in `chunked`, which the classic path rejects.
    TransferEncoding,
    /// A `Content-Length` that is duplicated, empty, longer than 18 digits or not only digits.
    ContentLength,
}

/// Outcome of [`scan_request`](crate::scan_request).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scan {
    /// More bytes could still make this a head.
    Partial,
    /// Not the lane's: the classic path decides.
    Miss(MissReason),
    /// A complete head, described in the [`RequestHead`](crate::RequestHead) that was passed in.
    Head,
}

/// Outcome of [`scan_response`](crate::scan_response).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanResponse {
    Partial,
    /// Not the common clean shape, or unsure: ask the classic parser. Not an error.
    Slow,
    /// A complete head, described in the [`ResponseHead`](crate::ResponseHead) that was passed in.
    Head,
}
