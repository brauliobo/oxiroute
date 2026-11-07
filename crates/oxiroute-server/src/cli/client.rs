#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) enum Endpoint {
    Tcp { authority: String, address: String },
    Unix(PathBuf),
}

impl Endpoint {
    pub(super) fn parse(value: &str) -> Result<Self, CliError> {
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

    pub(super) fn unix(path: &str) -> Result<Self, CliError> {
        if Path::new(path).is_absolute() {
            Ok(Self::Unix(PathBuf::from(path)))
        } else {
            Err(CliError::Local(
                "unix endpoint needs an absolute path".into(),
            ))
        }
    }

    /// One GET whose answer is returned whatever its status, to an authority the caller names.
    pub(super) fn get_unchecked(&self, path: &str, host: &str) -> Result<Response, CliError> {
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
    pub(super) const fn is_unix(&self) -> bool {
        matches!(self, Self::Unix(_))
    }

    fn connect(&self) -> Result<Stream, CliError> {
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

    fn host(&self) -> &str {
        match self {
            Self::Tcp { authority, .. } => authority,
            Self::Unix(_) => "localhost",
        }
    }
}

enum Stream {
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

pub(super) struct Client {
    pub(super) endpoint: Endpoint,
    pub(super) token: Option<Zeroizing<String>>,
}

impl Client {
    pub(super) fn get(&self, path: &str, authenticated: bool) -> Result<Response, CliError> {
        self.request("GET", path, &[], &[], authenticated)
    }

    pub(super) fn json(
        &self,
        method: &str,
        path: &str,
        value: &Value,
        headers: &[(&str, &str)],
    ) -> Result<Response, CliError> {
        let body = serde_json::to_vec(value).map_err(|error| CliError::Local(error.to_string()))?;
        self.request(method, path, &body, headers, true)
    }

    pub(super) fn mutation_json(
        &self,
        method: &str,
        path: &str,
        value: &Value,
    ) -> Result<Response, CliError> {
        let revision = self.active_revision()?;
        let mut value = value.clone();
        value
            .as_object_mut()
            .ok_or_else(|| CliError::Local("mutation body must be an object".into()))?
            .insert("expectedActiveRevision".into(), json!(revision));
        self.json(method, path, &value, &[])
    }

    pub(super) fn generation_json(
        &self,
        method: &str,
        path: &str,
        value: &Value,
    ) -> Result<Response, CliError> {
        let revision = self.active_revision()?;
        self.json(
            method,
            path,
            value,
            &[("If-Generation-Revision", &revision)],
        )
    }

    fn active_revision(&self) -> Result<String, CliError> {
        self.get("/api/v1/generations", true)?
            .json()?
            .pointer("/generation/activeRevision")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| CliError::Local("generation response has no active revision".into()))
    }

    pub(super) fn request(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        headers: &[(&str, &str)],
        authenticated: bool,
    ) -> Result<Response, CliError> {
        if !path.starts_with('/') || path.contains(['\r', '\n']) {
            return Err(CliError::Local("request path is invalid".into()));
        }
        let token = if authenticated && !self.endpoint.is_unix() {
            Some(self.token.as_deref().ok_or_else(|| {
                CliError::Local("management token file is missing or unreadable".into())
            })?)
        } else {
            None
        };
        let mut stream = self.endpoint.connect()?;
        write!(stream, "{method} {path} HTTP/1.1\r\nHost: {}\r\nAccept: application/json\r\nConnection: close\r\nContent-Length: {}\r\n", self.endpoint.host(), body.len()).map_err(connect_io)?;
        if !body.is_empty() {
            stream
                .write_all(b"Content-Type: application/json\r\n")
                .map_err(connect_io)?;
        }
        if let Some(token) = token {
            write!(stream, "Authorization: Bearer {token}\r\n").map_err(connect_io)?;
        }
        for (name, value) in headers {
            if name.contains(['\r', '\n']) || value.contains(['\r', '\n']) {
                return Err(CliError::Local("request header is invalid".into()));
            }
            write!(stream, "{name}: {value}\r\n").map_err(connect_io)?;
        }
        stream.write_all(b"\r\n").map_err(connect_io)?;
        stream.write_all(body).map_err(connect_io)?;
        Response::read(BufReader::new(stream))?.into_result()
    }
}

fn connect_io(_: io::Error) -> CliError {
    CliError::Connect("endpoint I/O failed".into())
}

#[derive(Clone, Copy)]
enum BodyFraming {
    None,
    Length(usize),
    Chunked,
    Close,
}

struct ParsedHead {
    status: u16,
    content_type: String,
    framing: BodyFraming,
}

impl ParsedHead {
    pub(super) fn parse(bytes: &[u8]) -> Result<Self, CliError> {
        let headers = std::str::from_utf8(bytes)
            .map_err(|_| CliError::Remote("invalid HTTP response headers".into()))?;
        let mut lines = headers.split("\r\n");
        let status_line = lines
            .next()
            .ok_or_else(|| CliError::Remote("invalid HTTP response status".into()))?;
        let mut status_fields = status_line.split_whitespace();
        if status_fields.next() != Some("HTTP/1.1") {
            return Err(CliError::Remote("endpoint did not return HTTP/1.1".into()));
        }
        let status: u16 = status_fields
            .next()
            .and_then(|value| value.parse().ok())
            .filter(|value| (100..=599).contains(value))
            .ok_or_else(|| CliError::Remote("invalid HTTP response status".into()))?;
        let mut content_type = String::new();
        let mut content_lengths = Vec::new();
        let mut transfer_codings = Vec::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            let (name, value) = line
                .split_once(':')
                .ok_or_else(|| CliError::Remote("invalid HTTP response header".into()))?;
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-type") {
                value.clone_into(&mut content_type);
            } else if name.eq_ignore_ascii_case("content-length") {
                for value in value.split(',') {
                    let length = value
                        .trim()
                        .parse::<usize>()
                        .map_err(|_| CliError::Remote("invalid HTTP Content-Length".into()))?;
                    content_lengths.push(length);
                }
            } else if name.eq_ignore_ascii_case("transfer-encoding") {
                transfer_codings.extend(
                    value
                        .split(',')
                        .map(|coding| coding.trim().to_ascii_lowercase()),
                );
            }
        }
        if content_lengths
            .windows(2)
            .any(|values| values[0] != values[1])
        {
            return Err(CliError::Remote(
                "conflicting HTTP Content-Length values".into(),
            ));
        }
        if !transfer_codings.is_empty() && !content_lengths.is_empty() {
            return Err(CliError::Remote(
                "HTTP response contains both Transfer-Encoding and Content-Length".into(),
            ));
        }
        let framing = if (100..200).contains(&status) || matches!(status, 204 | 304) {
            BodyFraming::None
        } else if !transfer_codings.is_empty() {
            if transfer_codings != ["chunked"] {
                return Err(CliError::Remote(
                    "unsupported HTTP Transfer-Encoding".into(),
                ));
            }
            BodyFraming::Chunked
        } else if let Some(length) = content_lengths.first().copied() {
            if length > MAX_RESPONSE_BYTES {
                return Err(CliError::Remote(
                    "response exceeded the client limit".into(),
                ));
            }
            BodyFraming::Length(length)
        } else {
            BodyFraming::Close
        };
        Ok(Self {
            status,
            content_type,
            framing,
        })
    }
}

fn read_http_head(reader: &mut impl BufRead) -> Result<Vec<u8>, CliError> {
    let mut head = Vec::new();
    loop {
        let mut line = Vec::new();
        let read = reader.read_until(b'\n', &mut line).map_err(connect_io)?;
        if read == 0 || !line.ends_with(b"\r\n") {
            return Err(CliError::Remote("truncated HTTP response headers".into()));
        }
        if head.len().saturating_add(line.len()) > MAX_HEADER_BYTES {
            return Err(CliError::Remote(
                "HTTP response headers exceeded the client limit".into(),
            ));
        }
        if line == b"\r\n" {
            return Ok(head);
        }
        head.extend_from_slice(&line);
    }
}

fn read_http_body(reader: &mut impl BufRead, framing: BodyFraming) -> Result<Vec<u8>, CliError> {
    match framing {
        BodyFraming::None => Ok(Vec::new()),
        BodyFraming::Length(length) => {
            let mut body = vec![0; length];
            reader
                .read_exact(&mut body)
                .map_err(|_| CliError::Remote("truncated HTTP response".into()))?;
            Ok(body)
        }
        BodyFraming::Chunked => read_chunked_body(reader),
        BodyFraming::Close => {
            let mut body = Vec::new();
            reader
                .take(u64::try_from(MAX_RESPONSE_BYTES + 1).expect("response bound fits u64"))
                .read_to_end(&mut body)
                .map_err(connect_io)?;
            if body.len() > MAX_RESPONSE_BYTES {
                return Err(CliError::Remote(
                    "response exceeded the client limit".into(),
                ));
            }
            Ok(body)
        }
    }
}

fn read_chunked_body(reader: &mut impl BufRead) -> Result<Vec<u8>, CliError> {
    let mut body = Vec::new();
    loop {
        let line = read_http_line(reader)?;
        let size = line
            .split(|byte| *byte == b';')
            .next()
            .and_then(|value| std::str::from_utf8(value).ok())
            .and_then(|value| usize::from_str_radix(value.trim(), 16).ok())
            .ok_or_else(|| CliError::Remote("invalid HTTP chunk size".into()))?;
        if size == 0 {
            let mut trailer_bytes = 0_usize;
            loop {
                let trailer = read_http_line(reader)?;
                trailer_bytes = trailer_bytes.saturating_add(trailer.len() + 2);
                if trailer_bytes > MAX_HEADER_BYTES {
                    return Err(CliError::Remote(
                        "HTTP trailers exceeded the client limit".into(),
                    ));
                }
                if trailer.is_empty() {
                    return Ok(body);
                }
                if !trailer.contains(&b':') {
                    return Err(CliError::Remote("invalid HTTP trailer".into()));
                }
            }
        }
        if size > MAX_RESPONSE_BYTES.saturating_sub(body.len()) {
            return Err(CliError::Remote(
                "response exceeded the client limit".into(),
            ));
        }
        let start = body.len();
        body.resize(start + size, 0);
        reader
            .read_exact(&mut body[start..])
            .map_err(|_| CliError::Remote("truncated HTTP chunk".into()))?;
        let mut terminator = [0; 2];
        reader
            .read_exact(&mut terminator)
            .map_err(|_| CliError::Remote("truncated HTTP chunk".into()))?;
        if terminator != *b"\r\n" {
            return Err(CliError::Remote("invalid HTTP chunk terminator".into()));
        }
    }
}

fn read_http_line(reader: &mut impl BufRead) -> Result<Vec<u8>, CliError> {
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line).map_err(connect_io)?;
    if !line.ends_with(b"\r\n") || line.len() > MAX_HEADER_BYTES {
        return Err(CliError::Remote("invalid HTTP framing line".into()));
    }
    line.truncate(line.len() - 2);
    Ok(line)
}

#[derive(Debug)]
pub(super) struct Response {
    pub(super) status: u16,
    pub(super) content_type: String,
    pub(super) body: Vec<u8>,
}

impl Response {
    #[cfg(test)]
    pub(super) fn parse(bytes: &[u8]) -> Result<Self, CliError> {
        Self::read(BufReader::new(Cursor::new(bytes)))
    }

    pub(super) fn read(mut reader: impl BufRead) -> Result<Self, CliError> {
        for _ in 0..=8 {
            let head = read_http_head(&mut reader)?;
            let parsed = ParsedHead::parse(&head)?;
            if (100..200).contains(&parsed.status) && parsed.status != 101 {
                continue;
            }
            let body = read_http_body(&mut reader, parsed.framing)?;
            return Ok(Self {
                status: parsed.status,
                content_type: parsed.content_type,
                body,
            });
        }
        Err(CliError::Remote(
            "endpoint returned too many interim responses".into(),
        ))
    }

    pub(super) fn into_result(self) -> Result<Self, CliError> {
        if (200..300).contains(&self.status) {
            Ok(self)
        } else {
            let value = serde_json::from_slice::<Value>(&self.body).ok();
            let code = value
                .as_ref()
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str)
                .map_or_else(|| format!("http_{}", self.status), str::to_owned);
            let code = value
                .as_ref()
                .and_then(|value| value.pointer("/diagnostics/0"))
                .and_then(|diagnostic| {
                    serde_json::from_value::<RejectionDiagnostic>(diagnostic.clone()).ok()
                })
                .map_or(code.clone(), |diagnostic| {
                    format!("{code}: {}", diagnostic.summary())
                });
            Err(CliError::Status(self.status, code))
        }
    }

    pub(super) fn json(&self) -> Result<Value, CliError> {
        serde_json::from_slice(&self.body)
            .map_err(|_| CliError::Remote("endpoint returned invalid JSON".into()))
    }

    pub(super) fn output_value(&self) -> Result<Value, CliError> {
        if self.content_type.starts_with("application/json") {
            self.json()
        } else {
            let text = std::str::from_utf8(&self.body)
                .map_err(|_| CliError::Remote("endpoint returned invalid UTF-8".into()))?;
            Ok(json!({ "contentType": self.content_type, "body": text }))
        }
    }
}
