#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone)]
pub(super) struct HttpOrigin {
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
    pub(super) fn parse(value: &str, policy: &RtmpOutboundPolicy) -> Result<Self, VodError> {
        let blueprint = HttpOriginBlueprint::parse(value)?;
        Self::acquire(&blueprint, policy)
    }

    pub(super) fn acquire(
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

    pub(super) fn fetch(&self, path: &str, maximum: u64) -> Result<Vec<u8>, VodError> {
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
    pub(super) fn parse(value: &str) -> Result<Self, VodError> {
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

#[derive(Clone, Debug)]
pub(super) struct HttpOriginBlueprint {
    scheme: HttpScheme,
    host: String,
    port: u16,
    base_path: String,
}
