use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    path::{Component, Path},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use http::Uri;
use openssl::ssl::{SslConnector, SslMethod, SslStream};
use rustix::{
    fd::OwnedFd,
    fs::{self as rustix_fs, FileType, Mode, OFlags},
};

use crate::{MediaEvent, RtmpOutboundPolicy};

pub const MAX_VOD_PATH_BYTES: usize = 4_096;
pub const MAX_VOD_SOURCE_NAME_BYTES: usize = 128;
pub const MAX_VOD_ORIGIN_BYTES: usize = 2_048;
pub const MAX_VOD_REDIRECTS: usize = 3;
pub const MAX_VOD_HTTP_HEADER_BYTES: usize = 64 * 1024;
pub const MAX_VOD_EVENTS: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VodLimits {
    pub max_sessions: usize,
    pub max_file_bytes: u64,
    pub max_duration: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VodSourceDefinition {
    Local {
        name: String,
        root_directory: std::path::PathBuf,
    },
    Http {
        name: String,
        origin: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum VodValueError {
    #[error("VOD field `{0}` is invalid")]
    InvalidField(&'static str),
}

impl VodLimits {
    /// # Errors
    ///
    /// Returns an error when a VOD session, file, or duration bound is zero.
    pub fn validate_intrinsic(self) -> Result<(), VodValueError> {
        if self.max_sessions == 0 || self.max_file_bytes == 0 || self.max_duration.is_zero() {
            return Err(VodValueError::InvalidField("limits"));
        }
        Ok(())
    }
}

impl VodSourceDefinition {
    /// # Errors
    ///
    /// Returns an error for an invalid source name, local root value, or HTTP origin syntax.
    pub fn validate_intrinsic(&self) -> Result<(), VodValueError> {
        let (name, valid_value) = match self {
            Self::Local {
                name,
                root_directory,
            } => (name, valid_absolute_path(root_directory)),
            Self::Http { name, origin } => (name, valid_http_origin_syntax(origin)),
        };
        if !valid_source_name(name) {
            return Err(VodValueError::InvalidField("source.name"));
        }
        if !valid_value {
            return Err(VodValueError::InvalidField("source.value"));
        }
        Ok(())
    }

    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Local { name, .. } | Self::Http { name, .. } => name,
        }
    }
}

fn valid_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str().len() <= MAX_VOD_PATH_BYTES
        && path.components().all(|component| {
            matches!(component, Component::RootDir | Component::Normal(_))
                && component.as_os_str().to_str().is_some_and(|value| {
                    !value
                        .bytes()
                        .any(|byte| byte == 0 || byte.is_ascii_control())
                })
        })
}

fn valid_http_origin_syntax(value: &str) -> bool {
    if value.len() > MAX_VOD_ORIGIN_BYTES {
        return false;
    }
    let Ok(uri) = value.parse::<Uri>() else {
        return false;
    };
    if !matches!(uri.scheme_str(), Some("http" | "https")) {
        return false;
    }
    let Some(authority) = uri.authority() else {
        return false;
    };
    let has_query = uri
        .path_and_query()
        .is_some_and(|path_and_query| path_and_query.query().is_some());
    !authority.as_str().contains('@')
        && !has_query
        && !value.contains('#')
        && !authority.host().is_empty()
        && authority.port_u16() != Some(0)
        && !uri.path().contains("..")
        && !uri.path().contains('%')
}

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum VodError {
    #[error("VOD source is not configured")]
    SourceNotFound,
    #[error("VOD source session limit reached")]
    SessionLimit,
    #[error("VOD path is invalid")]
    InvalidPath,
    #[error("VOD object was not found")]
    NotFound,
    #[error("VOD root cannot be opened safely")]
    RootOpen,
    #[error("VOD object exceeds its configured size bound")]
    TooLarge,
    #[error("VOD origin is not allowed")]
    OriginDenied,
    #[error("VOD origin request failed")]
    Fetch,
    #[error("VOD object is not a valid FLV stream")]
    InvalidFlv,
    #[error("VOD media payload is invalid")]
    InvalidMedia,
    #[error("VOD byte range is invalid")]
    InvalidRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VodRange {
    pub start: u64,
    pub end: u64,
}

impl VodRange {
    #[must_use]
    pub const fn full(size: u64) -> Option<Self> {
        if size == 0 {
            None
        } else {
            Some(Self {
                start: 0,
                end: size - 1,
            })
        }
    }

    /// Parses one RFC 9110 byte range. Multiple ranges are rejected to keep response memory and
    /// framing bounded to one contiguous body.
    ///
    /// # Errors
    ///
    /// Returns [`VodError::InvalidRange`] when the range is malformed, empty, unsatisfiable, or
    /// contains multiple ranges.
    pub fn parse(value: Option<&str>, size: u64) -> Result<Option<Self>, VodError> {
        let Some(value) = value else {
            return Ok(Self::full(size));
        };
        if size == 0 || !value.starts_with("bytes=") {
            return Err(VodError::InvalidRange);
        }
        let value = &value[6..];
        if value.contains(',') {
            return Err(VodError::InvalidRange);
        }
        let (start, end) = value.split_once('-').ok_or(VodError::InvalidRange)?;
        if start.is_empty() {
            let suffix = end.parse::<u64>().map_err(|_| VodError::InvalidRange)?;
            if suffix == 0 {
                return Err(VodError::InvalidRange);
            }
            let length = suffix.min(size);
            return Ok(Some(Self {
                start: size - length,
                end: size - 1,
            }));
        }
        let start = start.parse::<u64>().map_err(|_| VodError::InvalidRange)?;
        if start >= size {
            return Err(VodError::InvalidRange);
        }
        let end = if end.is_empty() {
            size - 1
        } else {
            end.parse::<u64>()
                .map_err(|_| VodError::InvalidRange)?
                .min(size - 1)
        };
        if end < start {
            return Err(VodError::InvalidRange);
        }
        Ok(Some(Self { start, end }))
    }

    #[must_use]
    pub const fn length(self) -> u64 {
        self.end - self.start + 1
    }
}

#[derive(Clone)]
struct VodRoot {
    fd: Arc<OwnedFd>,
}

impl VodRoot {
    fn open(path: &Path) -> Result<Self, VodError> {
        let fd = rustix_fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|_| VodError::RootOpen)?;
        let metadata = rustix_fs::fstat(&fd).map_err(|_| VodError::RootOpen)?;
        if !FileType::from_raw_mode(metadata.st_mode).is_dir() || metadata.st_mode & 0o022 != 0 {
            return Err(VodError::RootOpen);
        }
        Ok(Self { fd: Arc::new(fd) })
    }

    fn read(&self, path: &str, maximum: u64) -> Result<Vec<u8>, VodError> {
        let components = validated_path(path)?;
        let mut parent = None;
        let descriptor = components
            .iter()
            .enumerate()
            .try_fold(None, |_, (index, component)| {
                let directory = match parent.as_ref() {
                    Some(directory) => directory,
                    None => self.fd.as_ref(),
                };
                let flags = if index + 1 == components.len() {
                    OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW
                } else {
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW
                };
                let opened = rustix_fs::openat(directory, component.as_str(), flags, Mode::empty())
                    .map_err(|error| {
                        if error == rustix::io::Errno::NOENT {
                            VodError::NotFound
                        } else {
                            VodError::Fetch
                        }
                    })?;
                if index + 1 == components.len() {
                    Ok(Some(opened))
                } else {
                    parent = Some(opened);
                    Ok(None)
                }
            })?
            .ok_or(VodError::NotFound)?;
        let metadata = rustix_fs::fstat(&descriptor).map_err(|_| VodError::NotFound)?;
        if !FileType::from_raw_mode(metadata.st_mode).is_file() {
            return Err(VodError::NotFound);
        }
        let size = u64::try_from(metadata.st_size).map_err(|_| VodError::TooLarge)?;
        if size > maximum {
            return Err(VodError::TooLarge);
        }
        let mut file = File::from(descriptor);
        let mut bytes = Vec::with_capacity(usize::try_from(size).map_err(|_| VodError::TooLarge)?);
        file.read_to_end(&mut bytes).map_err(|_| VodError::Fetch)?;
        if u64::try_from(bytes.len()).map_err(|_| VodError::TooLarge)? > maximum {
            return Err(VodError::TooLarge);
        }
        Ok(bytes)
    }
}

#[derive(Clone)]
struct HttpOrigin {
    scheme: HttpScheme,
    host: String,
    port: u16,
    address: SocketAddr,
    base_path: String,
    policy: RtmpOutboundPolicy,
}

#[derive(Clone, Copy, Debug)]
enum HttpScheme {
    Http,
    Https,
}

impl HttpOrigin {
    fn parse(value: &str, policy: &RtmpOutboundPolicy) -> Result<Self, VodError> {
        let blueprint = HttpOriginBlueprint::parse(value)?;
        Self::acquire(&blueprint, policy)
    }

    fn acquire(
        blueprint: &HttpOriginBlueprint,
        policy: &RtmpOutboundPolicy,
    ) -> Result<Self, VodError> {
        let addresses: Vec<_> = (blueprint.host.as_str(), blueprint.port)
            .to_socket_addrs()
            .map_err(|_| VodError::OriginDenied)?
            .take(33)
            .collect();
        policy
            .validate_resolved(&blueprint.host, &addresses)
            .map_err(|_| VodError::OriginDenied)?;
        let address = addresses.first().copied().ok_or(VodError::OriginDenied)?;
        Ok(Self {
            scheme: blueprint.scheme,
            host: blueprint.host.clone(),
            port: blueprint.port,
            address,
            base_path: blueprint.base_path.clone(),
            policy: policy.clone(),
        })
    }

    fn fetch(&self, path: &str, maximum: u64) -> Result<Vec<u8>, VodError> {
        let mut origin = self.clone();
        for redirect in 0..=MAX_VOD_REDIRECTS {
            let target = join_http_path(&origin.base_path, path)?;
            let mut stream = connect_http(&origin)?;
            let host_header = if origin.port
                == match origin.scheme {
                    HttpScheme::Http => 80,
                    HttpScheme::Https => 443,
                } {
                origin.host.clone()
            } else {
                format!("{}:{}", origin.host, origin.port)
            };
            write!(
                stream,
                "GET {target} HTTP/1.1\r\nHost: {host_header}\r\nConnection: close\r\nAccept: video/x-flv, video/mp4, application/octet-stream\r\n\r\n"
            )
            .map_err(|_| VodError::Fetch)?;
            stream.flush().map_err(|_| VodError::Fetch)?;
            let response = read_http_response(stream, maximum)?;
            if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
                if redirect == MAX_VOD_REDIRECTS {
                    return Err(VodError::Fetch);
                }
                let location = response.location.ok_or(VodError::Fetch)?;
                origin = Self::parse(&location, &self.policy)?;
                continue;
            }
            if response.status != 200 && response.status != 206 {
                return Err(VodError::NotFound);
            }
            return response.body(maximum);
        }
        Err(VodError::Fetch)
    }
}

impl HttpOriginBlueprint {
    fn parse(value: &str) -> Result<Self, VodError> {
        if !valid_http_origin_syntax(value) {
            return Err(VodError::OriginDenied);
        }
        let uri = value
            .parse::<Uri>()
            .expect("validated HTTP origin syntax must parse");
        let scheme = match uri.scheme_str() {
            Some("http") => HttpScheme::Http,
            Some("https") => HttpScheme::Https,
            _ => return Err(VodError::OriginDenied),
        };
        let authority = uri
            .authority()
            .expect("validated HTTP origin syntax has an authority");
        let host = authority.host();
        let port = authority.port_u16().unwrap_or(match scheme {
            HttpScheme::Http => 80,
            HttpScheme::Https => 443,
        });
        Ok(Self {
            scheme,
            host: host.to_owned(),
            port,
            base_path: uri.path().trim_end_matches('/').to_owned(),
        })
    }
}

enum HttpStream {
    Plain(TcpStream),
    Tls(SslStream<TcpStream>),
}

delegate_read_write!(HttpStream { Plain, Tls });

fn connect_http(origin: &HttpOrigin) -> Result<HttpStream, VodError> {
    let stream = TcpStream::connect_timeout(&origin.address, Duration::from_secs(5))
        .map_err(|_| VodError::Fetch)?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|_| VodError::Fetch)?;
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|_| VodError::Fetch)?;
    match origin.scheme {
        HttpScheme::Http => Ok(HttpStream::Plain(stream)),
        HttpScheme::Https => {
            let connector = SslConnector::builder(SslMethod::tls_client())
                .map_err(|_| VodError::Fetch)?
                .build();
            connector
                .connect(&origin.host, stream)
                .map(HttpStream::Tls)
                .map_err(|_| VodError::Fetch)
        }
    }
}

struct HttpResponse {
    status: u16,
    content_length: Option<u64>,
    location: Option<String>,
    initial_body: Vec<u8>,
    stream: Option<HttpStream>,
}

impl HttpResponse {
    fn body(mut self, maximum: u64) -> Result<Vec<u8>, VodError> {
        if self.content_length.is_some_and(|length| length > maximum) {
            return Err(VodError::TooLarge);
        }
        let mut body = self.initial_body;
        if let Some(length) = self.content_length {
            let expected = usize::try_from(length).map_err(|_| VodError::TooLarge)?;
            if body.len() > expected {
                body.truncate(expected);
            }
            if body.len() < expected {
                let stream = self.stream.as_mut().ok_or(VodError::Fetch)?;
                let mut remaining = vec![0_u8; expected - body.len()];
                stream
                    .read_exact(&mut remaining)
                    .map_err(|_| VodError::Fetch)?;
                body.extend(remaining);
            }
        } else {
            let stream = self.stream.as_mut().ok_or(VodError::Fetch)?;
            let maximum = usize::try_from(maximum)
                .ok()
                .and_then(|maximum| maximum.checked_add(1))
                .ok_or(VodError::TooLarge)?;
            let mut buffer = [0_u8; 16 * 1024];
            while body.len() <= maximum {
                let read = stream.read(&mut buffer).map_err(|_| VodError::Fetch)?;
                if read == 0 {
                    break;
                }
                body.extend_from_slice(&buffer[..read]);
            }
            if body.len() > maximum - 1 {
                return Err(VodError::TooLarge);
            }
        }
        Ok(body)
    }
}

fn read_http_response(mut stream: HttpStream, maximum: u64) -> Result<HttpResponse, VodError> {
    let mut headers = Vec::with_capacity(1024);
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut buffer).map_err(|_| VodError::Fetch)?;
        if read == 0 {
            return Err(VodError::Fetch);
        }
        headers.extend_from_slice(&buffer[..read]);
        if let Some(index) = headers.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
        if headers.len() > MAX_VOD_HTTP_HEADER_BYTES {
            return Err(VodError::Fetch);
        }
    };
    let header_text = std::str::from_utf8(&headers[..header_end]).map_err(|_| VodError::Fetch)?;
    let mut lines = header_text.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or(VodError::Fetch)?;
    let mut content_length = None;
    let mut location = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                return Err(VodError::Fetch);
            }
            content_length = Some(value.trim().parse::<u64>().map_err(|_| VodError::Fetch)?);
        } else if name.eq_ignore_ascii_case("location") {
            if location.is_some() {
                return Err(VodError::Fetch);
            }
            location = Some(value.trim().to_owned());
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(VodError::Fetch);
        }
    }
    if content_length.is_some_and(|length| length > maximum) {
        return Err(VodError::TooLarge);
    }
    Ok(HttpResponse {
        status,
        content_length,
        location,
        initial_body: headers[header_end..].to_vec(),
        stream: Some(stream),
    })
}

fn join_http_path(base: &str, path: &str) -> Result<String, VodError> {
    let path = validated_path(path)?.join("/");
    if !path.is_ascii() {
        return Err(VodError::InvalidPath);
    }
    Ok(format!("{}/{}", base.trim_end_matches('/'), path))
}

fn validated_path(path: &str) -> Result<Vec<String>, VodError> {
    if path.is_empty()
        || path.len() > MAX_VOD_PATH_BYTES
        || path.starts_with('/')
        || path.contains("//")
    {
        return Err(VodError::InvalidPath);
    }
    let mut components = Vec::new();
    for component in Path::new(path).components() {
        let Component::Normal(component) = component else {
            return Err(VodError::InvalidPath);
        };
        let component = component.to_str().ok_or(VodError::InvalidPath)?;
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.contains(['/', '\\', '?', '#', '%'])
            || component.chars().any(char::is_control)
            || !component.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(VodError::InvalidPath);
        }
        components.push(component.to_owned());
    }
    if components.is_empty() {
        return Err(VodError::InvalidPath);
    }
    Ok(components)
}

pub struct VodObject {
    source: String,
    path: String,
    bytes: Arc<[u8]>,
    max_duration: Duration,
    _lease: VodLease,
}

impl VodObject {
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    #[must_use]
    pub fn len(&self) -> u64 {
        u64::try_from(self.bytes.len()).unwrap_or(u64::MAX)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub const fn max_duration(&self) -> Duration {
        self.max_duration
    }

    /// Returns one validated contiguous byte range from the object.
    ///
    /// # Errors
    ///
    /// Returns [`VodError::InvalidRange`] when the range does not fit this object.
    pub fn range(&self, range: VodRange) -> Result<Vec<u8>, VodError> {
        let start = usize::try_from(range.start).map_err(|_| VodError::InvalidRange)?;
        let end = usize::try_from(range.end).map_err(|_| VodError::InvalidRange)?;
        if end < start || end >= self.bytes.len() {
            return Err(VodError::InvalidRange);
        }
        Ok(self.bytes[start..=end].to_vec())
    }
}

pub struct VodApplication {
    service: String,
    application: String,
    limits: VodLimits,
    sources: BTreeMap<String, VodSource>,
    active_sessions: Arc<AtomicUsize>,
}

#[derive(Clone, Debug)]
pub struct VodApplicationBlueprint {
    service: String,
    application: String,
    limits: VodLimits,
    sources: BTreeMap<String, VodSourceBlueprint>,
    policy: RtmpOutboundPolicy,
}

#[derive(Clone, Debug)]
enum VodSourceBlueprint {
    Local(std::path::PathBuf),
    Http(HttpOriginBlueprint),
}

#[derive(Clone, Debug)]
struct HttpOriginBlueprint {
    scheme: HttpScheme,
    host: String,
    port: u16,
    base_path: String,
}

enum VodSource {
    Local(VodRoot),
    Http(HttpOrigin),
}

impl VodApplication {
    /// Creates a preflighted VOD application and pins all local roots and HTTP origins.
    ///
    /// # Errors
    ///
    /// Returns an error when a source root, source origin, limit, or source name is invalid.
    pub fn new(
        service: impl Into<String>,
        application: impl Into<String>,
        limits: VodLimits,
        sources: impl IntoIterator<Item = VodSourceDefinition>,
        policy: &RtmpOutboundPolicy,
    ) -> Result<Self, VodError> {
        let blueprint =
            VodApplicationBlueprint::compile(service, application, limits, sources, policy)?;
        Self::acquire(&blueprint)
    }

    /// Opens roots and HTTP clients for a compiled VOD application.
    ///
    /// # Errors
    ///
    /// Returns an acquisition error when a root or origin cannot be prepared.
    pub fn acquire(blueprint: &VodApplicationBlueprint) -> Result<Self, VodError> {
        let mut compiled = BTreeMap::new();
        for (name, source) in &blueprint.sources {
            let source = match source {
                VodSourceBlueprint::Local(root_directory) => {
                    VodSource::Local(VodRoot::open(root_directory)?)
                }
                VodSourceBlueprint::Http(origin) => {
                    VodSource::Http(HttpOrigin::acquire(origin, &blueprint.policy)?)
                }
            };
            compiled.insert(name.clone(), source);
        }
        Ok(Self {
            service: blueprint.service.clone(),
            application: blueprint.application.clone(),
            limits: blueprint.limits,
            sources: compiled,
            active_sessions: Arc::new(AtomicUsize::new(0)),
        })
    }
}

impl VodApplicationBlueprint {
    /// Compiles immutable VOD source and limit decisions without opening resources.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid limits, source identities, paths, or origin syntax.
    pub fn compile(
        service: impl Into<String>,
        application: impl Into<String>,
        limits: VodLimits,
        sources: impl IntoIterator<Item = VodSourceDefinition>,
        policy: &RtmpOutboundPolicy,
    ) -> Result<Self, VodError> {
        let mut compiled = BTreeMap::new();
        for source in sources {
            source
                .validate_intrinsic()
                .map_err(|_| VodError::InvalidPath)?;
            let name = source.name().to_owned();
            if !valid_source_name(&name) {
                return Err(VodError::InvalidPath);
            }
            let source = match source {
                VodSourceDefinition::Local { root_directory, .. } => {
                    VodSourceBlueprint::Local(root_directory)
                }
                VodSourceDefinition::Http { origin, .. } => {
                    VodSourceBlueprint::Http(HttpOriginBlueprint::parse(&origin)?)
                }
            };
            if compiled.insert(name, source).is_some() {
                return Err(VodError::SourceNotFound);
            }
        }
        if limits.max_sessions == 0 || limits.max_file_bytes == 0 || limits.max_duration.is_zero() {
            return Err(VodError::SessionLimit);
        }
        Ok(Self {
            service: service.into(),
            application: application.into(),
            limits,
            sources: compiled,
            policy: policy.clone(),
        })
    }
}

impl VodApplication {
    #[must_use]
    pub fn service(&self) -> &str {
        &self.service
    }

    #[must_use]
    pub fn application(&self) -> &str {
        &self.application
    }

    #[must_use]
    pub const fn limits(&self) -> VodLimits {
        self.limits
    }

    /// Opens one bounded VOD object and acquires its concurrent-session lease.
    ///
    /// # Errors
    ///
    /// Returns an error when the source/path is invalid, the object cannot be loaded, or the
    /// concurrent-session limit is reached.
    pub fn open(&self, source: &str, path: &str) -> Result<VodObject, VodError> {
        self.validate_request(source, path)?;
        let lease = self.reserve()?;
        let (bytes, max_duration) = self.load(source, path)?;
        Ok(VodObject {
            source: source.to_owned(),
            path: path.to_owned(),
            bytes: bytes.into(),
            max_duration,
            _lease: lease,
        })
    }

    pub(crate) fn validate_request(&self, source: &str, path: &str) -> Result<(), VodError> {
        if !valid_source_name(source) {
            return Err(VodError::InvalidPath);
        }
        if !self.sources.contains_key(source) {
            return Err(VodError::SourceNotFound);
        }
        validated_path(path).map(|_| ())
    }

    pub(crate) fn reserve(&self) -> Result<VodLease, VodError> {
        VodLease::acquire(Arc::clone(&self.active_sessions), self.limits.max_sessions)
    }

    pub(crate) fn load(&self, source: &str, path: &str) -> Result<(Vec<u8>, Duration), VodError> {
        let bytes = match self.sources.get(source).ok_or(VodError::SourceNotFound)? {
            VodSource::Local(root) => root.read(path, self.limits.max_file_bytes)?,
            VodSource::Http(origin) => origin.fetch(path, self.limits.max_file_bytes)?,
        };
        Ok((bytes, self.limits.max_duration))
    }
}

fn valid_source_name(value: &str) -> bool {
    (1..=MAX_VOD_SOURCE_NAME_BYTES).contains(&value.len())
        && value.bytes().all(|byte| byte.is_ascii_graphic())
        && !value.contains(['/', '\\', '?', '#', '%'])
        && value != "."
        && value != ".."
}

type VodApplicationMap = BTreeMap<(String, String), Arc<VodApplication>>;

#[derive(Clone, Default)]
pub struct VodCatalog {
    applications: Arc<Mutex<VodApplicationMap>>,
}

impl VodCatalog {
    #[must_use]
    pub(crate) fn from_applications(
        applications: impl IntoIterator<Item = Arc<VodApplication>>,
    ) -> Arc<Self> {
        let applications = applications
            .into_iter()
            .map(|application| {
                (
                    (application.service.clone(), application.application.clone()),
                    application,
                )
            })
            .collect();
        Arc::new(Self {
            applications: Arc::new(Mutex::new(applications)),
        })
    }

    /// Opens one object from the named service/application catalog entry.
    ///
    /// # Errors
    ///
    /// Returns an error when the service, application, source, path, or object is unavailable.
    ///
    /// # Panics
    ///
    /// Panics if the catalog mutex is poisoned.
    pub fn open(
        &self,
        service: &str,
        application: &str,
        source: &str,
        path: &str,
    ) -> Result<VodObject, VodError> {
        let applications = self
            .applications
            .lock()
            .expect("VOD catalog mutex poisoned");
        applications
            .get(&(service.to_owned(), application.to_owned()))
            .ok_or(VodError::SourceNotFound)?
            .open(source, path)
    }

    /// Returns a cloned application entry when it exists.
    ///
    /// # Panics
    ///
    /// Panics if the catalog mutex is poisoned.
    #[must_use]
    pub fn application(&self, service: &str, application: &str) -> Option<Arc<VodApplication>> {
        self.applications
            .lock()
            .expect("VOD catalog mutex poisoned")
            .get(&(service.to_owned(), application.to_owned()))
            .cloned()
    }
}

pub struct VodLease {
    active_sessions: Arc<AtomicUsize>,
}

impl VodLease {
    fn acquire(active_sessions: Arc<AtomicUsize>, maximum: usize) -> Result<Self, VodError> {
        let mut active = active_sessions.load(Ordering::Acquire);
        loop {
            if active >= maximum {
                return Err(VodError::SessionLimit);
            }
            match active_sessions.compare_exchange_weak(
                active,
                active + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(Self { active_sessions }),
                Err(current) => active = current,
            }
        }
    }
}

impl Drop for VodLease {
    fn drop(&mut self) {
        self.active_sessions.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) fn parse_flv_events(
    bytes: &[u8],
    maximum_duration: Duration,
) -> Result<Vec<MediaEvent>, VodError> {
    if bytes.len() < 13 || &bytes[..3] != b"FLV" || bytes[3] != 1 {
        return Err(VodError::InvalidFlv);
    }
    let data_offset = u32::from_be_bytes(bytes[5..9].try_into().expect("FLV header is checked"));
    let mut offset = usize::try_from(data_offset).map_err(|_| VodError::InvalidFlv)?;
    if offset < 9 || offset.checked_add(4).is_none_or(|end| end > bytes.len()) {
        return Err(VodError::InvalidFlv);
    }
    offset += 4;
    let maximum_ms = u32::try_from(maximum_duration.as_millis()).unwrap_or(u32::MAX);
    let mut events = Vec::new();
    while offset < bytes.len() {
        if offset.checked_add(11).is_none_or(|end| end > bytes.len()) {
            return Err(VodError::InvalidFlv);
        }
        let tag_type = bytes[offset];
        let data_size = usize::try_from(u32::from_be_bytes([
            0,
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]))
        .map_err(|_| VodError::InvalidFlv)?;
        let timestamp = u32::from_be_bytes([
            bytes[offset + 7],
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
        ]);
        let data_start = offset + 11;
        let data_end = data_start
            .checked_add(data_size)
            .and_then(|end| end.checked_add(4))
            .ok_or(VodError::InvalidFlv)?;
        if data_end > bytes.len() {
            return Err(VodError::InvalidFlv);
        }
        let previous_size = u32::from_be_bytes(
            bytes[data_start + data_size..data_end]
                .try_into()
                .expect("FLV previous tag size is checked"),
        );
        if previous_size != u32::try_from(data_size + 11).map_err(|_| VodError::InvalidFlv)? {
            return Err(VodError::InvalidFlv);
        }
        if timestamp <= maximum_ms {
            let payload: Arc<[u8]> = bytes[data_start..data_start + data_size].into();
            let event = match tag_type {
                8 => MediaEvent::audio(timestamp, payload).map_err(|_| VodError::InvalidMedia)?,
                9 => MediaEvent::video(timestamp, payload).map_err(|_| VodError::InvalidMedia)?,
                _ => {
                    offset = data_end;
                    continue;
                }
            };
            events.push(event);
            if events.len() > MAX_VOD_EVENTS {
                return Err(VodError::TooLarge);
            }
        }
        offset = data_end;
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use std::{fs, net::TcpListener, thread};

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn ranges_are_single_contiguous_and_bounded() {
        assert_eq!(
            VodRange::parse(Some("bytes=2-5"), 10),
            Ok(Some(VodRange { start: 2, end: 5 }))
        );
        assert_eq!(
            VodRange::parse(Some("bytes=-3"), 10),
            Ok(Some(VodRange { start: 7, end: 9 }))
        );
        assert_eq!(
            VodRange::parse(Some("bytes=9-"), 10),
            Ok(Some(VodRange { start: 9, end: 9 }))
        );
        assert!(matches!(
            VodRange::parse(Some("bytes=1-2,4-5"), 10),
            Err(VodError::InvalidRange)
        ));
    }

    #[test]
    fn local_source_rejects_traversal_and_symlinked_files() {
        let root = tempdir().expect("root");
        fs::write(root.path().join("movie.flv"), b"movie").expect("movie");
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.path().join("movie.flv"), root.path().join("link.flv"))
            .expect("symlink");
        let application = VodApplication::new(
            "live",
            "broadcast",
            VodLimits {
                max_sessions: 1,
                max_file_bytes: 1024,
                max_duration: Duration::from_mins(1),
            },
            [VodSourceDefinition::Local {
                name: "archive".into(),
                root_directory: root.path().to_path_buf(),
            }],
            &RtmpOutboundPolicy {
                deny_private: false,
                ..RtmpOutboundPolicy::default()
            },
        )
        .expect("VOD application");
        assert!(matches!(
            application.validate_request("missing", "movie.flv"),
            Err(VodError::SourceNotFound)
        ));
        assert!(matches!(
            application.validate_request("archive", "../movie.flv"),
            Err(VodError::InvalidPath)
        ));
        assert!(matches!(
            application.open("archive", "../movie.flv"),
            Err(VodError::InvalidPath)
        ));
        #[cfg(unix)]
        assert!(matches!(
            application.open("archive", "link.flv"),
            Err(VodError::Fetch)
        ));
        let object = application
            .open("archive", "movie.flv")
            .expect("movie object");
        assert_eq!(object.bytes(), b"movie");
    }

    #[test]
    fn local_source_enforces_session_limit() {
        let root = tempdir().expect("root");
        fs::write(root.path().join("movie.flv"), b"movie").expect("movie");
        let application = VodApplication::new(
            "live",
            "broadcast",
            VodLimits {
                max_sessions: 1,
                max_file_bytes: 1024,
                max_duration: Duration::from_mins(1),
            },
            [VodSourceDefinition::Local {
                name: "archive".into(),
                root_directory: root.path().to_path_buf(),
            }],
            &RtmpOutboundPolicy {
                deny_private: false,
                ..RtmpOutboundPolicy::default()
            },
        )
        .expect("VOD application");
        let first = application
            .open("archive", "movie.flv")
            .expect("first object");
        assert!(matches!(
            application.open("archive", "movie.flv"),
            Err(VodError::SessionLimit)
        ));
        drop(first);
        assert!(application.open("archive", "movie.flv").is_ok());
    }

    #[test]
    fn http_source_rejects_chunked_responses_and_respects_body_bound() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP listener");
        let address = listener.local_addr().expect("HTTP address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("HTTP request");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).expect("HTTP read");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                .expect("HTTP response");
        });
        let origin = format!("http://{address}");
        let result = HttpOrigin::parse(
            &origin,
            &RtmpOutboundPolicy {
                deny_private: false,
                ..RtmpOutboundPolicy::default()
            },
        )
        .expect("HTTP origin")
        .fetch("movie.flv", 1024);
        server.join().expect("HTTP server");
        assert!(matches!(result, Err(VodError::Fetch)));
    }

    #[test]
    fn http_source_rejects_ambiguous_content_length_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP listener");
        let address = listener.local_addr().expect("HTTP address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("HTTP request");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).expect("HTTP read");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nContent-Length: 3\r\n\r\nvod")
                .expect("HTTP response");
        });
        let origin = format!("http://{address}");
        let result = HttpOrigin::parse(
            &origin,
            &RtmpOutboundPolicy {
                deny_private: false,
                ..RtmpOutboundPolicy::default()
            },
        )
        .expect("HTTP origin")
        .fetch("movie.flv", 1024);
        server.join().expect("HTTP server");
        assert!(matches!(result, Err(VodError::Fetch)));
    }
}
