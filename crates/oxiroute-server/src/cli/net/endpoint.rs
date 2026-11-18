use std::{
    io::{self, BufReader, Write as _},
    net::{TcpStream, ToSocketAddrs as _},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::Duration,
};

use super::Response;
use crate::cli::error::CliError;

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
    pub(crate) fn get_unchecked(&self, path: &str, host: &str) -> Result<Response, CliError> {
        if path.contains(['\r', '\n', ' ']) || host.contains(['\r', '\n']) {
            return Err(CliError::Local("request target is invalid".into()));
        }
        let mut stream = self.connect()?;
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
        )
        .map_err(connect_io)?;
        Response::read(BufReader::new(stream))
    }

    /// Registry sockets authorize by file permission, so no token is read or sent.
    pub(crate) const fn is_unix(&self) -> bool {
        matches!(self, Self::Unix(_))
    }

    pub(super) fn connect(&self) -> Result<Stream, CliError> {
        let timeout = Some(Duration::from_secs(10));
        let connect = |error: io::Error| CliError::Connect(error.to_string());
        match self {
            Self::Tcp { address, .. } => {
                let address = address
                    .to_socket_addrs()
                    .map_err(|_| CliError::Connect("endpoint could not be resolved".into()))?
                    .next()
                    .ok_or_else(|| {
                        CliError::Connect("endpoint did not resolve to an address".into())
                    })?;
                let stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))
                    .map_err(|_| CliError::Connect("endpoint connection failed".into()))?;
                stream.set_read_timeout(timeout).map_err(connect)?;
                stream.set_write_timeout(timeout).map_err(connect)?;
                Ok(Stream::Tcp(stream))
            }
            Self::Unix(path) => {
                let stream = UnixStream::connect(path)
                    .map_err(|_| CliError::Connect("endpoint connection failed".into()))?;
                stream.set_read_timeout(timeout).map_err(connect)?;
                stream.set_write_timeout(timeout).map_err(connect)?;
                Ok(Stream::Unix(stream))
            }
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

pub(super) fn connect_io(_: io::Error) -> CliError {
    CliError::Connect("endpoint I/O failed".into())
}
