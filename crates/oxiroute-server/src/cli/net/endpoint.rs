use std::{
    fmt,
    io::{self, BufReader, Write as _},
    net::{TcpStream, ToSocketAddrs as _},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::Duration,
};

use super::{Response, Timeouts, span};
use crate::cli::error::CliError;

const ENDPOINT_HINT: &str = "the management endpoint is `management { bind }` in the configuration; \
pass it with --endpoint or OXIROUTE_ENDPOINT";

pub(crate) enum Endpoint {
    Tcp { authority: String, address: String },
    Unix(PathBuf),
}

impl Endpoint {
    pub(crate) fn parse(value: &str) -> Result<Self, CliError> {
        if let Some(path) = value.strip_prefix("unix:") {
            return Self::unix(path);
        }
        let authority = value
            .strip_prefix("http://")
            .ok_or_else(|| CliError::Local("endpoint must use http:// or unix:".into()))?;
        if authority.is_empty() || authority.contains('/') || authority.contains('@') {
            return Err(CliError::Local(
                "endpoint must contain only an HTTP authority".into(),
            ));
        }
        let address = if authority.starts_with('[') {
            let end = authority
                .find(']')
                .ok_or_else(|| CliError::Local("endpoint IPv6 authority is invalid".into()))?;
            let host = &authority[1..end];
            let port = authority
                .get(end + 1..)
                .and_then(|rest| rest.strip_prefix(':'))
                .unwrap_or("80");
            format!("[{host}]:{port}")
        } else if authority.rsplit_once(':').is_some() {
            authority.to_owned()
        } else {
            format!("{authority}:80")
        };
        Ok(Self::Tcp {
            authority: authority.to_owned(),
            address,
        })
    }

    pub(crate) fn unix(path: &str) -> Result<Self, CliError> {
        if Path::new(path).is_absolute() {
            Ok(Self::Unix(PathBuf::from(path)))
        } else {
            Err(CliError::Local(
                "unix endpoint needs an absolute path".into(),
            ))
        }
    }

    /// One GET whose answer is returned whatever its status, to an authority the caller names.
    pub(crate) fn get_unchecked(
        &self,
        path: &str,
        host: &str,
        timeouts: Timeouts,
    ) -> Result<Response, CliError> {
        if path.contains(['\r', '\n', ' ']) || host.contains(['\r', '\n']) {
            return Err(CliError::Local("request target is invalid".into()));
        }
        let mut stream = self.connect(timeouts)?;
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
        )
        .map_err(|error| self.transport(CliError::Io(error), timeouts))?;
        Response::read(BufReader::new(stream)).map_err(|error| self.transport(error, timeouts))
    }

    /// Registry sockets authorize by file permission, so no token is read or sent.
    pub(crate) const fn is_unix(&self) -> bool {
        matches!(self, Self::Unix(_))
    }

    pub(super) fn connect(&self, timeouts: Timeouts) -> Result<Stream, CliError> {
        let timeout = Some(timeouts.answer());
        let configure = |error: io::Error| self.transport(CliError::Io(error), timeouts);
        match self {
            Self::Tcp { address, .. } => {
                let address = address
                    .to_socket_addrs()
                    .ok()
                    .and_then(|mut addresses| addresses.next())
                    .ok_or_else(|| {
                        CliError::Connect(format!(
                            "cannot resolve {self}: the host does not resolve to an address; {ENDPOINT_HINT}"
                        ))
                    })?;
                let stream = TcpStream::connect_timeout(&address, timeouts.connect())
                    .map_err(|error| self.refused(&error, timeouts))?;
                stream.set_read_timeout(timeout).map_err(configure)?;
                stream.set_write_timeout(timeout).map_err(configure)?;
                Ok(Stream::Tcp(stream))
            }
            Self::Unix(path) => {
                let stream =
                    UnixStream::connect(path).map_err(|error| self.refused(&error, timeouts))?;
                stream.set_read_timeout(timeout).map_err(configure)?;
                stream.set_write_timeout(timeout).map_err(configure)?;
                Ok(Stream::Unix(stream))
            }
        }
    }

    fn refused(&self, error: &io::Error, timeouts: Timeouts) -> CliError {
        if error.kind() == io::ErrorKind::TimedOut {
            return CliError::Timeout(format!(
                "cannot connect to {self} within {}: the connection attempt timed out; check the address \
                 and any firewall, or raise the limit with --timeout",
                span(timeouts.connect())
            ));
        }
        CliError::Connect(format!(
            "cannot connect to {self}: {error}; is the daemon running? {ENDPOINT_HINT}"
        ))
    }

    pub(super) fn transport(&self, error: CliError, timeouts: Timeouts) -> CliError {
        match error {
            CliError::Io(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                CliError::Timeout(format!(
                    "{self} did not answer within {}: the daemon may be overloaded or stuck; \
                     raise the limit with --timeout",
                    span(timeouts.answer())
                ))
            }
            CliError::Io(error) => {
                CliError::Connect(format!("{self} stopped answering the request: {error}"))
            }
            other => other,
        }
    }

    pub(super) fn host(&self) -> &str {
        match self {
            Self::Tcp { authority, .. } => authority,
            Self::Unix(_) => "localhost",
        }
    }
}

pub(super) enum Stream {
    Tcp(TcpStream),
    Unix(UnixStream),
}

impl Stream {
    pub(super) fn set_read_timeout(&self, limit: Duration) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.set_read_timeout(Some(limit)),
            Self::Unix(stream) => stream.set_read_timeout(Some(limit)),
        }
    }
}

impl io::Read for Stream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.read(buffer),
            Self::Unix(stream) => stream.read(buffer),
        }
    }
}

impl io::Write for Stream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.write(bytes),
            Self::Unix(stream) => stream.write(bytes),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.flush(),
            Self::Unix(stream) => stream.flush(),
        }
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tcp { authority, .. } => write!(formatter, "http://{authority}"),
            Self::Unix(path) => write!(formatter, "unix:{}", path.display()),
        }
    }
}
