use std::env;
use std::fmt::Write as _;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::task::JoinSet;
use tokio::time::Instant;

const RESPONSE_BUFFER_BYTES: usize = 16 * 1024;

struct Config {
    implementation: String,
    host: String,
    port: u16,
    path: String,
    connections: usize,
    duration: Duration,
    expected_bytes: usize,
}

#[derive(Default)]
struct WorkerResult {
    requests: u64,
    response_bytes: u64,
    non_success: u64,
}

fn usage() -> &'static str {
    "usage: oxiroute-loadgen --implementation NAME --host HOST --port PORT \
--path PATH --connections COUNT --duration-seconds SECONDS --expected-bytes BYTES"
}

fn argument_value(arguments: &mut impl Iterator<Item = String>, name: &str) -> String {
    arguments
        .next()
        .unwrap_or_else(|| panic!("{name} requires a value"))
}

fn parse_positive<T>(value: &str, name: &str) -> T
where
    T: std::str::FromStr + PartialEq + Default,
{
    let parsed = value
        .parse::<T>()
        .unwrap_or_else(|_| panic!("{name} must be a positive integer"));
    assert!(parsed != T::default(), "{name} must be a positive integer");
    parsed
}

fn required<T>(value: Option<T>) -> T {
    value.unwrap_or_else(|| panic!("{}", usage()))
}

fn parse_config() -> Config {
    let mut implementation = None;
    let mut host = None;
    let mut port = None;
    let mut path = None;
    let mut connections = None;
    let mut duration_seconds = None;
    let mut expected_bytes = None;
    let mut arguments = env::args().skip(1);

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--implementation" => {
                implementation = Some(argument_value(&mut arguments, "--implementation"));
            }
            "--host" => host = Some(argument_value(&mut arguments, "--host")),
            "--port" => {
                port = Some(parse_positive(
                    &argument_value(&mut arguments, "--port"),
                    "--port",
                ));
            }
            "--path" => path = Some(argument_value(&mut arguments, "--path")),
            "--connections" => {
                connections = Some(parse_positive(
                    &argument_value(&mut arguments, "--connections"),
                    "--connections",
                ));
            }
            "--duration-seconds" => {
                duration_seconds = Some(parse_positive(
                    &argument_value(&mut arguments, "--duration-seconds"),
                    "--duration-seconds",
                ));
            }
            "--expected-bytes" => {
                expected_bytes = Some(parse_positive(
                    &argument_value(&mut arguments, "--expected-bytes"),
                    "--expected-bytes",
                ));
            }
            "--version" => {
                println!("oxiroute-loadgen {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            _ => panic!("unknown argument: {argument}"),
        }
    }

    let path = required(path);
    assert!(path.starts_with('/'), "--path must start with /");
    Config {
        implementation: required(implementation),
        host: required(host),
        port: required(port),
        path,
        connections: required(connections),
        duration: Duration::from_secs(required(duration_seconds)),
        expected_bytes: required(expected_bytes),
    }
}

fn find_header_end(response: &[u8]) -> Option<usize> {
    response.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_response_head(response: &[u8]) -> io::Result<(u16, usize)> {
    let text = std::str::from_utf8(response)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut lines = text.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing status line"))?;
    let mut status_parts = status_line.split_whitespace();
    if status_parts.next() != Some("HTTP/1.1") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "response protocol was not HTTP/1.1",
        ));
    }
    let status = status_parts
        .next()
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid response status"))?;
    let mut content_length = None;

    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            let length = value.parse::<usize>().map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid content-length: {error}"),
                )
            })?;
            if content_length.replace(length).is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "duplicate content-length",
                ));
            }
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "transfer-encoding responses are unsupported",
            ));
        } else if name.eq_ignore_ascii_case("connection") && value.eq_ignore_ascii_case("close") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "server disabled connection reuse",
            ));
        }
    }

    content_length
        .map(|length| (status, length))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing content-length"))
}

async fn read_response(
    stream: &mut TcpStream,
    buffer: &mut [u8; RESPONSE_BUFFER_BYTES],
    expected_bytes: usize,
) -> io::Result<u16> {
    let mut received = 0;
    let header_end = loop {
        if let Some(offset) = find_header_end(&buffer[..received]) {
            break offset;
        }
        if received == buffer.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "response headers exceed buffer",
            ));
        }
        let bytes = stream.read(&mut buffer[received..]).await?;
        if bytes == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "server closed a keep-alive connection",
            ));
        }
        received += bytes;
    };
    let body_start = header_end + 4;
    let (status, content_length) = parse_response_head(&buffer[..header_end])?;
    if content_length != expected_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected {expected_bytes} response bytes, received {content_length}"),
        ));
    }
    let response_length = body_start
        .checked_add(content_length)
        .filter(|length| *length <= buffer.len())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "response exceeds buffer"))?;
    while received < response_length {
        let bytes = stream.read(&mut buffer[received..response_length]).await?;
        if bytes == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "server closed before the response body completed",
            ));
        }
        received += bytes;
    }
    if received != response_length {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "received bytes beyond a non-pipelined response",
        ));
    }
    Ok(status)
}

async fn worker(
    mut stream: TcpStream,
    request: Arc<[u8]>,
    deadline: Instant,
    expected_bytes: usize,
) -> io::Result<WorkerResult> {
    let mut result = WorkerResult::default();
    let mut response = [0; RESPONSE_BUFFER_BYTES];
    while Instant::now() < deadline {
        stream.write_all(&request).await?;
        let status = read_response(&mut stream, &mut response, expected_bytes).await?;
        result.requests += 1;
        result.response_bytes += expected_bytes as u64;
        if !(200..400).contains(&status) {
            result.non_success += 1;
        }
    }
    Ok(result)
}

fn json_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                write!(escaped, "\\u{:04x}", character as u32)
                    .expect("writing to String cannot fail");
            }
            character => escaped.push(character),
        }
    }
    escaped
}

#[allow(clippy::cast_precision_loss)]
fn rate(value: u64, elapsed: f64) -> f64 {
    value as f64 / elapsed
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> io::Result<()> {
    let config = parse_config();
    let address = format!("{}:{}", config.host, config.port);
    let request_line = format!("GET {} HTTP/1.1", config.path);
    let request: Arc<[u8]> = format!(
        "{request_line}\r\nHost: {}:{}\r\nUser-Agent: oxiroute-loadgen/{}\r\nAccept: */*\r\nConnection: keep-alive\r\n\r\n",
        config.host,
        config.port,
        env!("CARGO_PKG_VERSION")
    )
    .into_bytes()
    .into();

    let mut connections = JoinSet::new();
    for _ in 0..config.connections {
        let address = address.clone();
        connections.spawn(async move {
            let stream = TcpStream::connect(address).await?;
            stream.set_nodelay(true)?;
            Ok::<_, io::Error>(stream)
        });
    }
    let mut streams = Vec::with_capacity(config.connections);
    while let Some(connection) = connections.join_next().await {
        streams.push(connection.map_err(io::Error::other)??);
    }

    let start = Instant::now();
    let deadline = start + config.duration;
    let mut workers = JoinSet::new();
    for stream in streams {
        workers.spawn(worker(
            stream,
            Arc::clone(&request),
            deadline,
            config.expected_bytes,
        ));
    }
    let mut total = WorkerResult::default();
    let mut failures = 0_u64;
    while let Some(worker_result) = workers.join_next().await {
        match worker_result.map_err(io::Error::other)? {
            Ok(result) => {
                total.requests += result.requests;
                total.response_bytes += result.response_bytes;
                total.non_success += result.non_success;
            }
            Err(error) => {
                failures += 1;
                eprintln!("load-generator worker failed: {error}");
            }
        }
    }
    let elapsed = start.elapsed().as_secs_f64();
    let requests_per_second = rate(total.requests, elapsed);
    let transfer_per_second = rate(total.response_bytes, elapsed);
    println!(
        concat!(
            "{{\n",
            "  \"schema\": \"oxiroute.local-v1.result.v1\",\n",
            "  \"implementation\": \"{}\",\n",
            "  \"load_generator\": \"oxiroute-loadgen/{}\",\n",
            "  \"protocol\": \"HTTP/1.1\",\n",
            "  \"request_line\": \"{}\",\n",
            "  \"connection_reuse\": \"keep-alive\",\n",
            "  \"connections\": {},\n",
            "  \"requests_per_second\": {:.3},\n",
            "  \"requests\": {},\n",
            "  \"elapsed_seconds\": {:.6},\n",
            "  \"transfer_bytes_per_second\": {:.3},\n",
            "  \"non_2xx_or_3xx\": {},\n",
            "  \"failed_requests\": {}\n",
            "}}"
        ),
        json_escape(&config.implementation),
        env!("CARGO_PKG_VERSION"),
        json_escape(&request_line),
        config.connections,
        requests_per_second,
        total.requests,
        elapsed,
        transfer_per_second,
        total.non_success,
        failures,
    );
    if failures != 0 || total.non_success != 0 {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{find_header_end, parse_response_head};

    #[test]
    fn parses_http_1_1_content_length_response() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 1024\r\nConnection: keep-alive";
        assert_eq!(parse_response_head(response).unwrap(), (200, 1024));
    }

    #[test]
    fn rejects_http_1_0_response() {
        let response = b"HTTP/1.0 200 OK\r\nContent-Length: 1024";
        assert!(parse_response_head(response).is_err());
    }

    #[test]
    fn finds_response_header_boundary() {
        assert_eq!(find_header_end(b"HTTP/1.1 200 OK\r\n\r\nbody"), Some(15));
    }
}
