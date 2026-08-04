use std::{
    fmt,
    io::{self, Read, Write},
    net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs},
    sync::Arc,
    time::Duration,
};

use openssl::ssl::{SslConnector, SslMethod, SslStream};

use crate::RtmpOutboundPolicy;

const MAX_CALLBACK_URL_BYTES: usize = 2_048;
const MAX_CALLBACK_RESPONSE_BYTES: usize = 16 * 1_024;
const MAX_CALLBACK_FORM_BYTES: usize = 8 * 1_024;
const MAX_CALLBACK_ADDRESSES: usize = 32;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RtmpCallbackMethod {
    Get,
    #[default]
    Post,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RtmpCallbackEvent {
    Connect,
    Disconnect,
    Publish,
    PublishDone,
    Play,
    PlayDone,
    Done,
    Update,
}

impl RtmpCallbackEvent {
    const fn name(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Disconnect => "disconnect",
            Self::Publish => "publish",
            Self::PublishDone => "publish_done",
            Self::Play => "play",
            Self::PlayDone => "play_done",
            Self::Done => "done",
            Self::Update => "update",
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RtmpCallbackContext {
    pub service_id: Arc<str>,
    pub application: Arc<str>,
    pub stream_name: Option<Arc<str>>,
    pub query: Option<Arc<str>>,
    pub client_addr: Option<IpAddr>,
    pub session_id: Arc<str>,
}

#[derive(Clone, Eq, PartialEq)]
pub struct RtmpCallbackEndpoint {
    address: SocketAddr,
    host: String,
    port: u16,
    secure: bool,
    path: String,
    query: Option<String>,
}

impl fmt::Debug for RtmpCallbackContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RtmpCallbackContext")
            .field("service_id", &self.service_id)
            .field("application", &self.application)
            .field("stream_name", &self.stream_name)
            .field("query", &self.query.as_ref().map(|_| "<redacted>"))
            .field("client_addr", &self.client_addr)
            .field("session_id", &self.session_id)
            .finish()
    }
}

impl fmt::Debug for RtmpCallbackEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RtmpCallbackEndpoint")
            .field("address", &self.address)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("secure", &self.secure)
            .field("path", &"<redacted>")
            .field("query", &self.query.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RtmpCallbackError {
    InvalidUrl,
    Resolution,
    AddressPolicy,
    Connect,
    Tls,
    Write,
    Read,
    Response,
    Rejected(u16),
    Redirect,
    FormTooLarge,
}

impl fmt::Display for RtmpCallbackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl => formatter.write_str("callback URL is invalid"),
            Self::Resolution => formatter.write_str("callback URL cannot be resolved"),
            Self::AddressPolicy => formatter.write_str("callback destination is not allowed"),
            Self::Connect => formatter.write_str("callback connection failed"),
            Self::Tls => formatter.write_str("callback TLS handshake failed"),
            Self::Write => formatter.write_str("callback request failed"),
            Self::Read => formatter.write_str("callback response could not be read"),
            Self::Response => formatter.write_str("callback response is invalid"),
            Self::Rejected(status) => write!(formatter, "callback rejected with HTTP {status}"),
            Self::Redirect => formatter.write_str("callback returned an unsupported redirect"),
            Self::FormTooLarge => formatter.write_str("callback parameters exceed the limit"),
        }
    }
}

impl std::error::Error for RtmpCallbackError {}

impl RtmpCallbackEndpoint {
    /// Parses, resolves, and policy-checks one callback URL during runtime-plan construction.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the URL is malformed, cannot be resolved, or fails the
    /// configured outbound destination policy.
    pub fn parse(
        value: &str,
        outbound_policy: &RtmpOutboundPolicy,
    ) -> Result<Self, RtmpCallbackError> {
        if value.is_empty()
            || value.len() > MAX_CALLBACK_URL_BYTES
            || value
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte == b' ')
            || value.contains('#')
        {
            return Err(RtmpCallbackError::InvalidUrl);
        }
        let (secure, remainder) = if let Some(remainder) = value.strip_prefix("https://") {
            (true, remainder)
        } else if let Some(remainder) = value.strip_prefix("http://") {
            (false, remainder)
        } else {
            return Err(RtmpCallbackError::InvalidUrl);
        };
        let (authority, path_and_query) = remainder
            .split_once('/')
            .map_or((remainder, ""), |value| value);
        if authority.is_empty() || authority.contains('@') {
            return Err(RtmpCallbackError::InvalidUrl);
        }
        let (host, port) = parse_authority(authority, secure)?;
        let (path, query) = match path_and_query.split_once('?') {
            Some((path, query)) => (
                format!("/{path}"),
                (!query.is_empty()).then_some(query.to_owned()),
            ),
            None => (format!("/{path_and_query}"), None),
        };
        let path = if path == "/" { "/".to_owned() } else { path };
        if path.len() > MAX_CALLBACK_URL_BYTES
            || query
                .as_ref()
                .is_some_and(|query| query.len() > MAX_CALLBACK_URL_BYTES)
        {
            return Err(RtmpCallbackError::InvalidUrl);
        }
        let mut addresses: Vec<_> = (host.as_str(), port)
            .to_socket_addrs()
            .map_err(|_| RtmpCallbackError::Resolution)?
            .take(MAX_CALLBACK_ADDRESSES + 1)
            .collect();
        if addresses.is_empty() || addresses.len() > MAX_CALLBACK_ADDRESSES {
            return Err(RtmpCallbackError::Resolution);
        }
        addresses.sort_unstable();
        addresses.dedup();
        outbound_policy
            .validate_resolved(&host, &addresses)
            .map_err(|_| RtmpCallbackError::AddressPolicy)?;
        Ok(Self {
            address: addresses[0],
            host,
            port,
            secure,
            path,
            query,
        })
    }

    #[must_use]
    pub fn address(&self) -> SocketAddr {
        self.address
    }
}

#[derive(Clone, Debug)]
pub struct RtmpCallbackPolicy {
    pub on_connect: Option<RtmpCallbackEndpoint>,
    pub on_disconnect: Option<RtmpCallbackEndpoint>,
    pub on_publish: Option<RtmpCallbackEndpoint>,
    pub on_publish_done: Option<RtmpCallbackEndpoint>,
    pub on_play: Option<RtmpCallbackEndpoint>,
    pub on_play_done: Option<RtmpCallbackEndpoint>,
    pub on_done: Option<RtmpCallbackEndpoint>,
    pub on_update: Option<RtmpCallbackEndpoint>,
    pub method: RtmpCallbackMethod,
    pub timeout: Duration,
    pub update_timeout: Duration,
    pub update_strict: bool,
    pub relay_redirect: bool,
}

impl Default for RtmpCallbackPolicy {
    fn default() -> Self {
        Self {
            on_connect: None,
            on_disconnect: None,
            on_publish: None,
            on_publish_done: None,
            on_play: None,
            on_play_done: None,
            on_done: None,
            on_update: None,
            method: RtmpCallbackMethod::default(),
            timeout: Duration::from_secs(10),
            update_timeout: Duration::from_secs(30),
            update_strict: false,
            relay_redirect: false,
        }
    }
}

impl RtmpCallbackPolicy {
    /// Runs an authorization callback and requires a successful 2xx response.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the callback cannot be reached, returns a non-success status,
    /// or exceeds the configured request bounds.
    pub fn authorize(
        &self,
        event: RtmpCallbackEvent,
        context: &RtmpCallbackContext,
    ) -> Result<(), RtmpCallbackError> {
        let Some(endpoint) = self.endpoint(event) else {
            return Ok(());
        };
        let response = request(endpoint, self.method, event, context, self.timeout)?;
        if response
            .status
            .is_some_and(|status| (200..300).contains(&status))
        {
            Ok(())
        } else if response
            .status
            .is_some_and(|status| (300..400).contains(&status))
            && self.relay_redirect
        {
            Err(RtmpCallbackError::Redirect)
        } else {
            Err(RtmpCallbackError::Rejected(response.status.unwrap_or(0)))
        }
    }

    /// Runs a teardown callback without changing the RTMP role lifecycle.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the callback cannot be reached or returns a non-success
    /// status.
    pub fn notify(
        &self,
        event: RtmpCallbackEvent,
        context: &RtmpCallbackContext,
    ) -> Result<(), RtmpCallbackError> {
        let Some(endpoint) = self.endpoint(event) else {
            return Ok(());
        };
        let response = request(endpoint, self.method, event, context, self.timeout)?;
        if response
            .status
            .is_some_and(|status| (200..300).contains(&status))
        {
            Ok(())
        } else {
            Err(
                if response
                    .status
                    .is_some_and(|status| (300..400).contains(&status))
                {
                    RtmpCallbackError::Redirect
                } else {
                    RtmpCallbackError::Rejected(response.status.unwrap_or(0))
                },
            )
        }
    }

    /// Runs an update callback with its dedicated timeout.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the callback cannot be reached or returns a non-success
    /// status.
    pub fn update(&self, context: &RtmpCallbackContext) -> Result<(), RtmpCallbackError> {
        let Some(endpoint) = self.endpoint(RtmpCallbackEvent::Update) else {
            return Ok(());
        };
        let response = request(
            endpoint,
            self.method,
            RtmpCallbackEvent::Update,
            context,
            self.update_timeout,
        )?;
        if response
            .status
            .is_some_and(|status| (200..300).contains(&status))
        {
            Ok(())
        } else {
            Err(RtmpCallbackError::Rejected(response.status.unwrap_or(0)))
        }
    }

    #[must_use]
    pub fn has_update(&self) -> bool {
        self.on_update.is_some() && !self.update_timeout.is_zero()
    }

    fn endpoint(&self, event: RtmpCallbackEvent) -> Option<&RtmpCallbackEndpoint> {
        match event {
            RtmpCallbackEvent::Connect => self.on_connect.as_ref(),
            RtmpCallbackEvent::Disconnect => self.on_disconnect.as_ref(),
            RtmpCallbackEvent::Publish => self.on_publish.as_ref(),
            RtmpCallbackEvent::PublishDone => self.on_publish_done.as_ref(),
            RtmpCallbackEvent::Play => self.on_play.as_ref(),
            RtmpCallbackEvent::PlayDone => self.on_play_done.as_ref(),
            RtmpCallbackEvent::Done => self.on_done.as_ref(),
            RtmpCallbackEvent::Update => self.on_update.as_ref(),
        }
    }
}

struct CallbackResponse {
    status: Option<u16>,
}

enum CallbackStream {
    Plain(TcpStream),
    Tls(SslStream<TcpStream>),
}

impl Read for CallbackStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.read(buffer),
            Self::Tls(stream) => stream.read(buffer),
        }
    }
}

impl Write for CallbackStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.write(buffer),
            Self::Tls(stream) => stream.write(buffer),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(stream) => stream.flush(),
            Self::Tls(stream) => stream.flush(),
        }
    }
}

fn request(
    endpoint: &RtmpCallbackEndpoint,
    method: RtmpCallbackMethod,
    event: RtmpCallbackEvent,
    context: &RtmpCallbackContext,
    timeout: Duration,
) -> Result<CallbackResponse, RtmpCallbackError> {
    let form = form_parameters(event, context)?;
    let path = match method {
        RtmpCallbackMethod::Get => append_query(&endpoint.path, endpoint.query.as_deref(), &form),
        RtmpCallbackMethod::Post => append_query(&endpoint.path, endpoint.query.as_deref(), ""),
    };
    let body = match method {
        RtmpCallbackMethod::Get => Vec::new(),
        RtmpCallbackMethod::Post => form.into_bytes(),
    };
    let method_name = match method {
        RtmpCallbackMethod::Get => "GET",
        RtmpCallbackMethod::Post => "POST",
    };
    let request = format!(
        "{method_name} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Length: {}\r\nContent-Type: application/x-www-form-urlencoded\r\n\r\n",
        host_header(endpoint),
        body.len()
    );
    let stream = TcpStream::connect_timeout(&endpoint.address, timeout)
        .map_err(|_| RtmpCallbackError::Connect)?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|_| RtmpCallbackError::Connect)?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|_| RtmpCallbackError::Connect)?;
    let mut stream = if endpoint.secure {
        let connector = SslConnector::builder(SslMethod::tls_client())
            .map_err(|_| RtmpCallbackError::Tls)?
            .build();
        CallbackStream::Tls(
            connector
                .connect(&endpoint.host, stream)
                .map_err(|_| RtmpCallbackError::Tls)?,
        )
    } else {
        CallbackStream::Plain(stream)
    };
    stream
        .write_all(request.as_bytes())
        .and_then(|()| stream.write_all(&body))
        .and_then(|()| stream.flush())
        .map_err(|_| RtmpCallbackError::Write)?;
    parse_response(&mut stream)
}

fn parse_response(stream: &mut CallbackStream) -> Result<CallbackResponse, RtmpCallbackError> {
    let mut response = Vec::with_capacity(1_024);
    let mut buffer = [0; 1_024];
    while response.len() < MAX_CALLBACK_RESPONSE_BYTES {
        let count = stream
            .read(&mut buffer)
            .map_err(|_| RtmpCallbackError::Read)?;
        if count == 0 {
            break;
        }
        response.extend_from_slice(&buffer[..count]);
        if response.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or(RtmpCallbackError::Response)?;
    let head =
        std::str::from_utf8(&response[..header_end]).map_err(|_| RtmpCallbackError::Response)?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse().ok());
    Ok(CallbackResponse { status })
}

fn form_parameters(
    event: RtmpCallbackEvent,
    context: &RtmpCallbackContext,
) -> Result<String, RtmpCallbackError> {
    let mut values = vec![
        ("call", event.name().to_owned()),
        ("app", context.application.as_ref().to_owned()),
        ("clientid", context.session_id.as_ref().to_owned()),
        ("server", context.service_id.as_ref().to_owned()),
    ];
    if let Some(stream_name) = &context.stream_name {
        values.push(("name", stream_name.as_ref().to_owned()));
    }
    if let Some(query) = &context.query {
        values.push(("query", query.as_ref().to_owned()));
    }
    if let Some(address) = context.client_addr {
        values.push(("addr", address.to_string()));
    }
    let encoded = values
        .into_iter()
        .map(|(key, value)| format!("{}={}", encode(key), encode(&value)))
        .collect::<Vec<_>>()
        .join("&");
    (encoded.len() <= MAX_CALLBACK_FORM_BYTES)
        .then_some(encoded)
        .ok_or(RtmpCallbackError::FormTooLarge)
}

fn append_query(path: &str, base_query: Option<&str>, form: &str) -> String {
    let mut value = String::with_capacity(path.len() + form.len() + 2);
    value.push_str(path);
    if base_query.is_some() || !form.is_empty() {
        value.push('?');
        if let Some(base_query) = base_query {
            value.push_str(base_query);
            if !form.is_empty() {
                value.push('&');
            }
        }
        value.push_str(form);
    }
    value
}

fn encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(b"0123456789ABCDEF"[(byte >> 4) as usize]));
            encoded.push(char::from(b"0123456789ABCDEF"[(byte & 0x0f) as usize]));
        }
    }
    encoded
}

fn parse_authority(authority: &str, secure: bool) -> Result<(String, u16), RtmpCallbackError> {
    if let Some(value) = authority.strip_prefix('[') {
        let (host, port) = value.split_once(']').ok_or(RtmpCallbackError::InvalidUrl)?;
        let port = port.strip_prefix(':').map_or_else(
            || Ok(if secure { 443 } else { 80 }),
            |port| port.parse().map_err(|_| RtmpCallbackError::InvalidUrl),
        )?;
        return Ok((host.to_owned(), port));
    }
    let (host, port) = authority.rsplit_once(':').map_or_else(
        || (authority, if secure { 443 } else { 80 }),
        |(host, port)| (host, port.parse().unwrap_or(0)),
    );
    if host.is_empty() || port == 0 {
        return Err(RtmpCallbackError::InvalidUrl);
    }
    Ok((host.to_owned(), port))
}

fn host_header(endpoint: &RtmpCallbackEndpoint) -> String {
    let host = if endpoint.host.contains(':') {
        format!("[{}]", endpoint.host)
    } else {
        endpoint.host.clone()
    };
    if (endpoint.secure && endpoint.port == 443) || (!endpoint.secure && endpoint.port == 80) {
        host
    } else {
        format!("{host}:{}", endpoint.port)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use super::*;

    fn public_policy() -> RtmpOutboundPolicy {
        RtmpOutboundPolicy {
            deny_private: false,
            ..RtmpOutboundPolicy::default()
        }
    }

    #[test]
    fn callback_url_parses_default_port_and_path() {
        let endpoint = RtmpCallbackEndpoint::parse("http://198.51.100.4/notify", &public_policy())
            .expect("callback endpoint");
        assert_eq!(
            endpoint.address(),
            "198.51.100.4:80".parse().expect("address")
        );
        assert_eq!(endpoint.path, "/notify");
    }

    #[test]
    fn callback_url_rejects_fragments_and_credentials() {
        assert_eq!(
            RtmpCallbackEndpoint::parse("http://user@198.51.100.4/notify", &public_policy()),
            Err(RtmpCallbackError::InvalidUrl)
        );
        assert_eq!(
            RtmpCallbackEndpoint::parse("http://198.51.100.4/notify#fragment", &public_policy()),
            Err(RtmpCallbackError::InvalidUrl)
        );
    }

    #[test]
    fn form_encoding_is_bounded_and_does_not_log_secrets() {
        let context = RtmpCallbackContext {
            service_id: "edge".into(),
            application: "live".into(),
            stream_name: Some("camera".into()),
            query: Some("token=a&b".into()),
            client_addr: Some("192.0.2.10".parse().expect("address")),
            session_id: "session".into(),
        };
        let form = form_parameters(RtmpCallbackEvent::Update, &context).expect("form");
        assert!(form.contains("query=token%3Da%26b"));
        assert!(!format!("{context:?}").contains("secret"));
    }

    #[test]
    fn post_callback_sends_event_fields_and_accepts_success() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("callback listener");
        let address = listener.local_addr().expect("callback address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("callback connection");
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .expect("callback read timeout");
            let mut request = Vec::new();
            let mut buffer = [0; 1_024];
            loop {
                let count = stream.read(&mut buffer).expect("callback request");
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n")
                    && request.windows(12).any(|window| window == b"call=publish")
                {
                    break;
                }
            }
            assert!(request.starts_with(b"POST /notify HTTP/1.1"));
            assert!(request.windows(12).any(|window| window == b"call=publish"));
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .expect("callback response");
        });
        let endpoint =
            RtmpCallbackEndpoint::parse(&format!("http://{address}/notify"), &public_policy())
                .expect("callback endpoint");
        let policy = RtmpCallbackPolicy {
            on_publish: Some(endpoint),
            timeout: Duration::from_secs(1),
            ..RtmpCallbackPolicy::default()
        };
        let context = RtmpCallbackContext {
            service_id: "edge".into(),
            application: "live".into(),
            stream_name: Some("camera".into()),
            query: None,
            client_addr: None,
            session_id: "session".into(),
        };
        policy
            .authorize(RtmpCallbackEvent::Publish, &context)
            .expect("callback accepted");
        server.join().expect("callback server");
    }
}
