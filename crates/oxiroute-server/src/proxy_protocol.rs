use std::{
    fmt,
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use oxiroute_config::{ProxyProtocolPolicy, ProxyProtocolVersion};
use serde::Serialize;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf},
    sync::watch,
    time::timeout,
};

pub const MAX_V1_HEADER_BYTES: usize = 108;
pub const MAX_V2_PAYLOAD_BYTES: usize = u16::MAX as usize;
pub const MAX_V2_HEADER_BYTES: usize = 16 + MAX_V2_PAYLOAD_BYTES;

const V1_SIGNATURE: &[u8] = b"PROXY ";
const V2_SIGNATURE: &[u8; 12] = b"\r\n\r\n\0\r\nQUIT\n";
const V2_HEADER_BYTES: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProxyProtocolTransport {
    Stream,
    Datagram,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyProtocolResult {
    Accepted,
    Sent,
    Timeout,
    Cancelled,
    Malformed,
    Unsupported,
    Mismatch,
    IoError,
}

impl ProxyProtocolResult {
    pub(crate) const ALL: [Self; 8] = [
        Self::Accepted,
        Self::Sent,
        Self::Timeout,
        Self::Cancelled,
        Self::Malformed,
        Self::Unsupported,
        Self::Mismatch,
        Self::IoError,
    ];

    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Accepted => 0,
            Self::Sent => 1,
            Self::Timeout => 2,
            Self::Cancelled => 3,
            Self::Malformed => 4,
            Self::Unsupported => 5,
            Self::Mismatch => 6,
            Self::IoError => 7,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Sent => "sent",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Malformed => "malformed",
            Self::Unsupported => "unsupported",
            Self::Mismatch => "mismatch",
            Self::IoError => "io_error",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProxyProtocolErrorKind {
    Cancelled,
    Timeout,
    UnexpectedEof,
    InvalidSignature,
    HeaderTooLarge,
    InvalidVersion,
    UnsupportedCommand,
    UnsupportedFamily,
    ProtocolMismatch,
    InvalidAddress,
    InvalidPort,
    InvalidLength,
    Io,
}

impl ProxyProtocolErrorKind {
    #[must_use]
    pub const fn result(self) -> ProxyProtocolResult {
        match self {
            Self::Cancelled => ProxyProtocolResult::Cancelled,
            Self::Timeout => ProxyProtocolResult::Timeout,
            Self::UnsupportedCommand | Self::UnsupportedFamily => {
                ProxyProtocolResult::Unsupported
            }
            Self::ProtocolMismatch => ProxyProtocolResult::Mismatch,
            Self::Io => ProxyProtocolResult::IoError,
            Self::UnexpectedEof
            | Self::InvalidSignature
            | Self::HeaderTooLarge
            | Self::InvalidVersion
            | Self::InvalidAddress
            | Self::InvalidPort
            | Self::InvalidLength => ProxyProtocolResult::Malformed,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::UnexpectedEof => "unexpected_eof",
            Self::InvalidSignature => "invalid_signature",
            Self::HeaderTooLarge => "header_too_large",
            Self::InvalidVersion => "invalid_version",
            Self::UnsupportedCommand => "unsupported_command",
            Self::UnsupportedFamily => "unsupported_family",
            Self::ProtocolMismatch => "protocol_mismatch",
            Self::InvalidAddress => "invalid_address",
            Self::InvalidPort => "invalid_port",
            Self::InvalidLength => "invalid_length",
            Self::Io => "io_error",
        }
    }
}

#[derive(Debug)]
pub struct ProxyProtocolError {
    kind: ProxyProtocolErrorKind,
    source: Option<io::Error>,
}

impl ProxyProtocolError {
    #[must_use]
    pub const fn new(kind: ProxyProtocolErrorKind) -> Self {
        Self {
            kind,
            source: None,
        }
    }

    #[must_use]
    pub fn io(source: io::Error) -> Self {
        Self {
            kind: ProxyProtocolErrorKind::Io,
            source: Some(source),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> ProxyProtocolErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn result(&self) -> ProxyProtocolResult {
        self.kind.result()
    }
}

impl fmt::Display for ProxyProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "PROXY protocol {}", self.kind.as_str())
    }
}

impl std::error::Error for ProxyProtocolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParsedProxyHeader {
    pub version: ProxyProtocolVersion,
    pub source: SocketAddr,
    pub destination: SocketAddr,
    pub consumed: usize,
}

/// Parses one complete header from the beginning of `input`.
///
/// `Ok(None)` means the input is a valid prefix but does not contain the complete header yet. No
/// parser path allocates based on input-controlled lengths; v2 is capped by its 16-bit protocol
/// payload length and v1 is capped by its 108-byte wire limit.
pub fn parse_header(
    input: &[u8],
    configured_version: ProxyProtocolVersion,
    transport: ProxyProtocolTransport,
) -> Result<Option<ParsedProxyHeader>, ProxyProtocolError> {
    if input.is_empty() {
        return Ok(None);
    }
    match configured_version {
        ProxyProtocolVersion::V1 => parse_v1(input, transport),
        ProxyProtocolVersion::V2 => parse_v2(input, transport),
        ProxyProtocolVersion::Auto => {
            if input[0] == V1_SIGNATURE[0] {
                parse_v1(input, transport)
            } else if input[0] == V2_SIGNATURE[0] {
                parse_v2(input, transport)
            } else {
                Err(ProxyProtocolError::new(
                    ProxyProtocolErrorKind::InvalidSignature,
                ))
            }
        }
    }
}

fn parse_v1(
    input: &[u8],
    transport: ProxyProtocolTransport,
) -> Result<Option<ParsedProxyHeader>, ProxyProtocolError> {
    if transport != ProxyProtocolTransport::Stream {
        return Err(ProxyProtocolError::new(
            ProxyProtocolErrorKind::ProtocolMismatch,
        ));
    }
    if input.len() < V1_SIGNATURE.len() {
        return if V1_SIGNATURE.starts_with(input) {
            Ok(None)
        } else {
            Err(ProxyProtocolError::new(
                ProxyProtocolErrorKind::InvalidSignature,
            ))
        };
    }
    if !input.starts_with(V1_SIGNATURE) {
        if input.starts_with(V2_SIGNATURE) {
            return Err(ProxyProtocolError::new(
                ProxyProtocolErrorKind::ProtocolMismatch,
            ));
        }
        return Err(ProxyProtocolError::new(
            ProxyProtocolErrorKind::InvalidSignature,
        ));
    }
    let line_end = input.windows(2).position(|window| window == b"\r\n");
    let Some(line_end) = line_end else {
        return if input.len() >= MAX_V1_HEADER_BYTES {
            Err(ProxyProtocolError::new(
                ProxyProtocolErrorKind::HeaderTooLarge,
            ))
        } else {
            Ok(None)
        };
    };
    let consumed = line_end + 2;
    if consumed > MAX_V1_HEADER_BYTES {
        return Err(ProxyProtocolError::new(
            ProxyProtocolErrorKind::HeaderTooLarge,
        ));
    }
    let line = std::str::from_utf8(&input[..line_end]).map_err(|_| {
        ProxyProtocolError::new(ProxyProtocolErrorKind::InvalidSignature)
    })?;
    let fields = line.split(' ').collect::<Vec<_>>();
    if fields.len() == 2 && fields[0] == "PROXY" && fields[1] == "UNKNOWN" {
        return Err(ProxyProtocolError::new(
            ProxyProtocolErrorKind::UnsupportedCommand,
        ));
    }
    if fields.len() != 6 || fields[0] != "PROXY" {
        return Err(ProxyProtocolError::new(
            ProxyProtocolErrorKind::InvalidSignature,
        ));
    }
    let is_v4 = match fields[1] {
        "TCP4" => true,
        "TCP6" => false,
        "UNKNOWN" => {
            return Err(ProxyProtocolError::new(
                ProxyProtocolErrorKind::UnsupportedCommand,
            ));
        }
        _ => {
            return Err(ProxyProtocolError::new(
                ProxyProtocolErrorKind::UnsupportedFamily,
            ));
        }
    };
    let source = parse_socket_addr(fields[2], fields[4], is_v4)?;
    let destination = parse_socket_addr(fields[3], fields[5], is_v4)?;
    Ok(Some(ParsedProxyHeader {
        version: ProxyProtocolVersion::V1,
        source,
        destination,
        consumed,
    }))
}

fn parse_v2(
    input: &[u8],
    transport: ProxyProtocolTransport,
) -> Result<Option<ParsedProxyHeader>, ProxyProtocolError> {
    if input.len() < V2_SIGNATURE.len() {
        return if V2_SIGNATURE.starts_with(input) {
            Ok(None)
        } else {
            Err(ProxyProtocolError::new(
                ProxyProtocolErrorKind::InvalidSignature,
            ))
        };
    }
    if !input.starts_with(V2_SIGNATURE) {
        if input.starts_with(V1_SIGNATURE) {
            return Err(ProxyProtocolError::new(
                ProxyProtocolErrorKind::ProtocolMismatch,
            ));
        }
        return Err(ProxyProtocolError::new(
            ProxyProtocolErrorKind::InvalidSignature,
        ));
    }
    if input.len() < V2_HEADER_BYTES {
        return Ok(None);
    }
    let version_command = input[12];
    if version_command >> 4 != 2 {
        return Err(ProxyProtocolError::new(
            ProxyProtocolErrorKind::InvalidVersion,
        ));
    }
    if version_command & 0x0f != 1 {
        return Err(ProxyProtocolError::new(
            ProxyProtocolErrorKind::UnsupportedCommand,
        ));
    }
    let family_protocol = input[13];
    let family = family_protocol >> 4;
    let protocol = family_protocol & 0x0f;
    let is_v4 = match family {
        1 => true,
        2 => false,
        _ => {
            return Err(ProxyProtocolError::new(
                ProxyProtocolErrorKind::UnsupportedFamily,
            ));
        }
    };
    let expected_protocol = match transport {
        ProxyProtocolTransport::Stream => 1,
        ProxyProtocolTransport::Datagram => 2,
    };
    if protocol != expected_protocol {
        return Err(ProxyProtocolError::new(
            ProxyProtocolErrorKind::ProtocolMismatch,
        ));
    }
    let payload_len = usize::from(u16::from_be_bytes([input[14], input[15]]));
    let address_len = if is_v4 { 12 } else { 36 };
    if payload_len < address_len {
        return Err(ProxyProtocolError::new(
            ProxyProtocolErrorKind::InvalidLength,
        ));
    }
    let consumed = V2_HEADER_BYTES + payload_len;
    if input.len() < consumed {
        return Ok(None);
    }
    let address = &input[V2_HEADER_BYTES..V2_HEADER_BYTES + address_len];
    let (source, destination) = if is_v4 {
        let source = IpAddr::V4(Ipv4Addr::new(
            address[0], address[1], address[2], address[3],
        ));
        let destination = IpAddr::V4(Ipv4Addr::new(
            address[4], address[5], address[6], address[7],
        ));
        (
            SocketAddr::new(source, u16::from_be_bytes([address[8], address[9]])),
            SocketAddr::new(destination, u16::from_be_bytes([address[10], address[11]])),
        )
    } else {
        let source_octets: [u8; 16] = address[..16]
            .try_into()
            .expect("v2 IPv6 source has a fixed 16-byte shape");
        let destination_octets: [u8; 16] = address[16..32]
            .try_into()
            .expect("v2 IPv6 destination has a fixed 16-byte shape");
        let source = IpAddr::V6(Ipv6Addr::from(source_octets));
        let destination = IpAddr::V6(Ipv6Addr::from(destination_octets));
        (
            SocketAddr::new(source, u16::from_be_bytes([address[32], address[33]])),
            SocketAddr::new(destination, u16::from_be_bytes([address[34], address[35]])),
        )
    };
    Ok(Some(ParsedProxyHeader {
        version: ProxyProtocolVersion::V2,
        source,
        destination,
        consumed,
    }))
}

fn parse_socket_addr(
    address: &str,
    port: &str,
    is_v4: bool,
) -> Result<SocketAddr, ProxyProtocolError> {
    let address = address
        .parse::<IpAddr>()
        .map_err(|_| ProxyProtocolError::new(ProxyProtocolErrorKind::InvalidAddress))?;
    if address.is_ipv4() != is_v4 {
        return Err(ProxyProtocolError::new(
            ProxyProtocolErrorKind::UnsupportedFamily,
        ));
    }
    let port = port
        .parse::<u16>()
        .map_err(|_| ProxyProtocolError::new(ProxyProtocolErrorKind::InvalidPort))?;
    Ok(SocketAddr::new(address, port))
}

/// Encodes a PROXY header without touching the source socket address.
pub fn encode_header(
    version: ProxyProtocolVersion,
    transport: ProxyProtocolTransport,
    source: SocketAddr,
    destination: SocketAddr,
) -> Result<Vec<u8>, ProxyProtocolError> {
    if source.is_ipv4() != destination.is_ipv4() {
        return Err(ProxyProtocolError::new(
            ProxyProtocolErrorKind::ProtocolMismatch,
        ));
    }
    match version {
        ProxyProtocolVersion::V1 => {
            if transport != ProxyProtocolTransport::Stream {
                return Err(ProxyProtocolError::new(
                    ProxyProtocolErrorKind::ProtocolMismatch,
                ));
            }
            let family = if source.is_ipv4() { "TCP4" } else { "TCP6" };
            let value = format!(
                "PROXY {family} {} {} {} {}\r\n",
                source.ip(),
                destination.ip(),
                source.port(),
                destination.port()
            );
            let bytes = value.into_bytes();
            if bytes.len() > MAX_V1_HEADER_BYTES {
                return Err(ProxyProtocolError::new(
                    ProxyProtocolErrorKind::HeaderTooLarge,
                ));
            }
            Ok(bytes)
        }
        ProxyProtocolVersion::V2 => {
            let family = match (source.is_ipv4(), transport) {
                (true, ProxyProtocolTransport::Stream) => 0x11,
                (true, ProxyProtocolTransport::Datagram) => 0x12,
                (false, ProxyProtocolTransport::Stream) => 0x21,
                (false, ProxyProtocolTransport::Datagram) => 0x22,
            };
            let address_len = if source.is_ipv4() { 12 } else { 36 };
            let mut bytes = Vec::with_capacity(V2_HEADER_BYTES + address_len);
            bytes.extend_from_slice(V2_SIGNATURE);
            bytes.extend_from_slice(&[0x21, family]);
            bytes.extend_from_slice(&(address_len as u16).to_be_bytes());
            match (source.ip(), destination.ip()) {
                (IpAddr::V4(source), IpAddr::V4(destination)) => {
                    bytes.extend_from_slice(&source.octets());
                    bytes.extend_from_slice(&destination.octets());
                }
                (IpAddr::V6(source), IpAddr::V6(destination)) => {
                    bytes.extend_from_slice(&source.octets());
                    bytes.extend_from_slice(&destination.octets());
                }
                _ => unreachable!("address families were checked above"),
            }
            bytes.extend_from_slice(&source.port().to_be_bytes());
            bytes.extend_from_slice(&destination.port().to_be_bytes());
            Ok(bytes)
        }
        ProxyProtocolVersion::Auto => Err(ProxyProtocolError::new(
            ProxyProtocolErrorKind::ProtocolMismatch,
        )),
    }
}

pub struct AcceptedProxyStream<S> {
    pub stream: PrefixedStream<S>,
    pub header: ParsedProxyHeader,
}

pub struct PrefixedStream<S> {
    prefix: Vec<u8>,
    prefix_offset: usize,
    inner: S,
}

impl<S> PrefixedStream<S> {
    fn new(inner: S, prefix: Vec<u8>) -> Self {
        Self {
            prefix,
            prefix_offset: 0,
            inner,
        }
    }

    #[must_use]
    pub fn into_inner(self) -> S {
        self.inner
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for PrefixedStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.prefix_offset < self.prefix.len() {
            let remaining = &self.prefix[self.prefix_offset..];
            let count = remaining.len().min(buffer.remaining());
            buffer.put_slice(&remaining[..count]);
            self.prefix_offset += count;
            Poll::Ready(Ok(()))
        } else {
            Pin::new(&mut self.inner).poll_read(context, buffer)
        }
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for PrefixedStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(context, buffer)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(context)
    }
}

pub async fn accept_stream<S>(
    stream: S,
    policy: ProxyProtocolPolicy,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<AcceptedProxyStream<S>, ProxyProtocolError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    timeout(
        Duration::from_millis(policy.timeout_ms),
        accept_stream_inner(stream, policy.version, shutdown),
    )
    .await
    .map_err(|_| ProxyProtocolError::new(ProxyProtocolErrorKind::Timeout))?
}

async fn accept_stream_inner<S>(
    mut stream: S,
    version: ProxyProtocolVersion,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<AcceptedProxyStream<S>, ProxyProtocolError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut collected = Vec::with_capacity(MAX_V2_HEADER_BYTES);
    let mut scratch = [0_u8; MAX_V2_HEADER_BYTES];
    loop {
        let read_limit = read_limit(&collected, version).ok_or_else(|| {
            ProxyProtocolError::new(ProxyProtocolErrorKind::HeaderTooLarge)
        })?;
        let read = tokio::select! {
            biased;
            () = wait_for_shutdown(shutdown) => {
                return Err(ProxyProtocolError::new(ProxyProtocolErrorKind::Cancelled));
            }
            result = stream.read(&mut scratch[..read_limit]) => result.map_err(ProxyProtocolError::io)?,
        };
        if read == 0 {
            return Err(ProxyProtocolError::new(
                ProxyProtocolErrorKind::UnexpectedEof,
            ));
        }
        collected.extend_from_slice(&scratch[..read]);
        if let Some(header) = parse_header(&collected, version, ProxyProtocolTransport::Stream)? {
            let prefix = collected[header.consumed..].to_vec();
            return Ok(AcceptedProxyStream {
                stream: PrefixedStream::new(stream, prefix),
                header,
            });
        }
    }
}

fn read_limit(input: &[u8], version: ProxyProtocolVersion) -> Option<usize> {
    let maximum = match version {
        ProxyProtocolVersion::V1 => MAX_V1_HEADER_BYTES,
        ProxyProtocolVersion::V2 | ProxyProtocolVersion::Auto => MAX_V2_HEADER_BYTES,
    };
    let expected = if input.len() >= V2_HEADER_BYTES && input.starts_with(V2_SIGNATURE) {
        let payload_len = usize::from(u16::from_be_bytes([input[14], input[15]]));
        V2_HEADER_BYTES.checked_add(payload_len)?
    } else {
        maximum
    };
    expected.checked_sub(input.len()).filter(|remaining| *remaining > 0)
}

async fn wait_for_shutdown(shutdown: &mut watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow_and_update() {
            return;
        }
        if shutdown.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt, duplex},
        sync::watch,
        time::{Duration, timeout},
    };

    use super::*;

    const V4_SOURCE: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)), 1234);
    const V4_DESTINATION: SocketAddr =
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 20)), 443);
    const V6_SOURCE: SocketAddr =
        SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 1234);
    const V6_DESTINATION: SocketAddr =
        SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 8443);

    fn policy(version: ProxyProtocolVersion) -> ProxyProtocolPolicy {
        ProxyProtocolPolicy {
            version,
            timeout_ms: 100,
        }
    }

    #[test]
    fn parses_v1_and_preserves_its_wire_limit() {
        let bytes = b"PROXY TCP4 192.0.2.10 198.51.100.20 1234 443\r\npayload";
        let header = parse_header(
            bytes,
            ProxyProtocolVersion::V1,
            ProxyProtocolTransport::Stream,
        )
        .expect("v1 parse")
        .expect("complete header");
        assert_eq!(header.source, V4_SOURCE);
        assert_eq!(header.destination, V4_DESTINATION);
        assert_eq!(&bytes[header.consumed..], b"payload");
        assert!(header.consumed <= MAX_V1_HEADER_BYTES);
    }

    #[test]
    fn parses_v2_stream_and_datagram_families() {
        for (transport, source, destination) in [
            (ProxyProtocolTransport::Stream, V4_SOURCE, V4_DESTINATION),
            (ProxyProtocolTransport::Datagram, V4_SOURCE, V4_DESTINATION),
            (ProxyProtocolTransport::Stream, V6_SOURCE, V6_DESTINATION),
            (ProxyProtocolTransport::Datagram, V6_SOURCE, V6_DESTINATION),
        ] {
            let encoded = encode_header(
                ProxyProtocolVersion::V2,
                transport,
                source,
                destination,
            )
            .expect("v2 encode");
            let parsed = parse_header(&encoded, ProxyProtocolVersion::V2, transport)
                .expect("v2 parse")
                .expect("complete header");
            assert_eq!(parsed.version, ProxyProtocolVersion::V2);
            assert_eq!(parsed.source, source);
            assert_eq!(parsed.destination, destination);
            assert_eq!(parsed.consumed, encoded.len());
        }
    }

    #[test]
    fn rejects_local_unix_unknown_and_mismatched_forms() {
        let mut local = encode_header(
            ProxyProtocolVersion::V2,
            ProxyProtocolTransport::Stream,
            V4_SOURCE,
            V4_DESTINATION,
        )
        .expect("header");
        local[12] = 0x20;
        assert_eq!(
            parse_header(
                &local,
                ProxyProtocolVersion::V2,
                ProxyProtocolTransport::Stream
            )
            .expect_err("LOCAL must fail")
            .kind(),
            ProxyProtocolErrorKind::UnsupportedCommand
        );

        let mut unix = local;
        unix[12] = 0x21;
        unix[13] = 0x31;
        assert_eq!(
            parse_header(
                &unix,
                ProxyProtocolVersion::V2,
                ProxyProtocolTransport::Stream
            )
            .expect_err("UNIX must fail")
            .kind(),
            ProxyProtocolErrorKind::UnsupportedFamily
        );

        let unknown = b"PROXY UNKNOWN\r\n";
        assert_eq!(
            parse_header(
                unknown,
                ProxyProtocolVersion::V1,
                ProxyProtocolTransport::Stream
            )
            .expect_err("UNKNOWN must fail")
            .kind(),
            ProxyProtocolErrorKind::UnsupportedCommand
        );
        assert_eq!(
            parse_header(
                b"PROXY TCP4 192.0.2.1 198.51.100.1 1 2\r\n",
                ProxyProtocolVersion::V1,
                ProxyProtocolTransport::Datagram
            )
            .expect_err("v1 UDP must fail")
            .kind(),
            ProxyProtocolErrorKind::ProtocolMismatch
        );
    }

    #[tokio::test]
    async fn accepts_overread_and_returns_application_bytes() {
        let (mut client, server) = duplex(256);
        let (shutdown_tx, mut shutdown) = watch::channel(false);
        let task = tokio::spawn(async move {
            accept_stream(server, policy(ProxyProtocolVersion::V1), &mut shutdown)
                .await
                .expect("accept header")
        });
        client
            .write_all(b"PROXY TCP4 192.0.2.10 198.51.100.20 1234 443\r\nhello")
            .await
            .expect("write header and payload");
        let accepted = timeout(Duration::from_secs(1), task)
            .await
            .expect("accept timeout")
            .expect("accept task");
        let mut payload = Vec::new();
        accepted
            .stream
            .take(5)
            .read_to_end(&mut payload)
            .await
            .expect("read payload");
        assert_eq!(payload, b"hello");
        drop(shutdown_tx);
    }

    #[tokio::test]
    async fn header_read_timeout_is_bounded() {
        let (_client, server) = duplex(16);
        let (_shutdown_tx, mut shutdown) = watch::channel(false);
        let error = accept_stream(server, policy(ProxyProtocolVersion::V2), &mut shutdown)
            .await
            .err()
            .expect("incomplete header must time out");
        assert_eq!(error.kind(), ProxyProtocolErrorKind::Timeout);
    }
}
