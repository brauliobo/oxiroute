//! HTTP/3 (RFC 9114 section 8.1) and QPACK (RFC 9204 section 6) error codes.
use std::fmt;

/// An application error code this layer sends.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Code(u32);

macro_rules! codes {
    ($($name:ident = $code:literal;)*) => {
        impl Code {
            $(pub const $name: Code = Code($code);)*

            fn name(self) -> Option<&'static str> {
                match self.0 { $($code => Some(stringify!($name)),)* _ => None }
            }
        }
    };
}

codes! {
    H3_NO_ERROR = 0x100;
    H3_GENERAL_PROTOCOL_ERROR = 0x101;
    H3_INTERNAL_ERROR = 0x102;
    H3_STREAM_CREATION_ERROR = 0x103;
    H3_CLOSED_CRITICAL_STREAM = 0x104;
    H3_FRAME_UNEXPECTED = 0x105;
    H3_FRAME_ERROR = 0x106;
    H3_EXCESSIVE_LOAD = 0x107;
    H3_ID_ERROR = 0x108;
    H3_SETTINGS_ERROR = 0x109;
    H3_MISSING_SETTINGS = 0x10a;
    H3_REQUEST_REJECTED = 0x10b;
    H3_REQUEST_CANCELLED = 0x10c;
    H3_REQUEST_INCOMPLETE = 0x10d;
    H3_MESSAGE_ERROR = 0x10e;
    H3_CONNECT_ERROR = 0x10f;
    H3_VERSION_FALLBACK = 0x110;
    QPACK_DECOMPRESSION_FAILED = 0x200;
    QPACK_ENCODER_STREAM_ERROR = 0x201;
    QPACK_DECODER_STREAM_ERROR = 0x202;
}

impl Code {
    /// The wire value.
    #[must_use]
    pub fn value(self) -> u64 {
        u64::from(self.0)
    }

    /// The code as a QUIC variable-length integer.
    #[must_use]
    pub fn varint(self) -> quinn::VarInt {
        quinn::VarInt::from_u32(self.0)
    }
}

impl fmt::Debug for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(name) => f.write_str(name),
            None => write!(f, "Code({:#x})", self.0),
        }
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}
