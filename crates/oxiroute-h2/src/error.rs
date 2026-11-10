//! Error codes (RFC 9113 section 7) and the error type returned by the connection and stream handles.
use std::fmt;
use std::io;

/// HTTP/2 error code as carried by `RST_STREAM` and `GOAWAY`; unknown codes are representable.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Reason(u32);

macro_rules! reasons {
    ($($name:ident = $code:literal, $text:literal;)*) => {
        impl Reason {
            $(pub const $name: Reason = Reason($code);)*

            /// Short description of the code.
            #[must_use]
            pub fn description(self) -> &'static str {
                match self.0 { $($code => $text,)* _ => "unknown reason" }
            }

            fn name(self) -> Option<&'static str> {
                match self.0 { $($code => Some(stringify!($name)),)* _ => None }
            }
        }
    };
}

reasons! {
    NO_ERROR = 0, "not a result of an error";
    PROTOCOL_ERROR = 1, "unspecific protocol error detected";
    INTERNAL_ERROR = 2, "unexpected internal error encountered";
    FLOW_CONTROL_ERROR = 3, "flow-control protocol violated";
    SETTINGS_TIMEOUT = 4, "settings ACK not received in timely manner";
    STREAM_CLOSED = 5, "received frame when stream half-closed";
    FRAME_SIZE_ERROR = 6, "frame with invalid size";
    REFUSED_STREAM = 7, "refused stream before processing any application logic";
    CANCEL = 8, "stream no longer needed";
    COMPRESSION_ERROR = 9, "unable to maintain the header compression context";
    CONNECT_ERROR = 10, "connection established in response to a CONNECT request was reset or abnormally closed";
    ENHANCE_YOUR_CALM = 11, "detected excessive load generating behavior";
    INADEQUATE_SECURITY = 12, "security properties do not meet minimum requirements";
    HTTP_1_1_REQUIRED = 13, "endpoint requires HTTP/1.1";
}

impl Reason {
    /// The wire value.
    #[must_use]
    pub fn code(self) -> u32 {
        self.0
    }
}

impl From<u32> for Reason {
    fn from(code: u32) -> Self {
        Reason(code)
    }
}

impl From<Reason> for u32 {
    fn from(reason: Reason) -> u32 {
        reason.0
    }
}

impl fmt::Debug for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(name) => f.write_str(name),
            None => write!(f, "Reason({})", self.0),
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.description())
    }
}

/// Who caused a reset or `GOAWAY`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Initiator {
    /// The peer sent it.
    Remote,
    /// This crate sent it because of a protocol violation by the peer.
    Library,
    /// The application asked for it.
    User,
}

#[derive(Clone, Debug)]
enum Kind {
    Reset(Reason, Initiator),
    GoAway(Reason, Initiator),
    Io(io::ErrorKind),
    /// Misuse of the stream handle by the application.
    User(&'static str),
}

/// Error of a connection or stream operation. Cheap to clone so it can be reported to every waiter.
#[derive(Clone, Debug)]
pub struct Error(Kind);

impl Error {
    pub(crate) fn reset(reason: Reason, by: Initiator) -> Self {
        Self(Kind::Reset(reason, by))
    }

    pub(crate) fn go_away(reason: Reason, by: Initiator) -> Self {
        Self(Kind::GoAway(reason, by))
    }

    pub(crate) fn user(what: &'static str) -> Self {
        Self(Kind::User(what))
    }

    pub(crate) fn io(err: &io::Error) -> Self {
        Self(Kind::Io(err.kind()))
    }

    pub(crate) fn closed() -> Self {
        Self(Kind::Io(io::ErrorKind::BrokenPipe))
    }

    /// The error code, when the error is a reset or a `GOAWAY`.
    #[must_use]
    pub fn reason(&self) -> Option<Reason> {
        match self.0 {
            Kind::Reset(r, _) | Kind::GoAway(r, _) => Some(r),
            _ => None,
        }
    }

    #[must_use]
    pub fn is_reset(&self) -> bool {
        matches!(self.0, Kind::Reset(..))
    }

    #[must_use]
    pub fn is_go_away(&self) -> bool {
        matches!(self.0, Kind::GoAway(..))
    }

    #[must_use]
    pub fn is_io(&self) -> bool {
        matches!(self.0, Kind::Io(_))
    }

    /// The reset or `GOAWAY` came from the peer.
    #[must_use]
    pub fn is_remote(&self) -> bool {
        matches!(
            self.0,
            Kind::Reset(_, Initiator::Remote) | Kind::GoAway(_, Initiator::Remote)
        )
    }

    /// This crate closed the stream or connection because of what the peer sent.
    #[must_use]
    pub fn is_library(&self) -> bool {
        matches!(
            self.0,
            Kind::Reset(_, Initiator::Library) | Kind::GoAway(_, Initiator::Library)
        )
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let who = |by| match by {
            Initiator::Remote => "received from peer",
            Initiator::Library => "sent by library",
            Initiator::User => "sent by user",
        };
        match &self.0 {
            Kind::Reset(r, by) => write!(f, "stream error {}: {r}", who(*by)),
            Kind::GoAway(r, by) => write!(f, "connection error {}: {r}", who(*by)),
            Kind::Io(kind) => write!(f, "connection closed: {kind}"),
            Kind::User(what) => f.write_str(what),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Self::io(&err)
    }
}
