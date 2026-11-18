use std::io::{BufRead, Read as _};

use serde_json::{Value, json};

use super::endpoint::connect_io;
use crate::{cli::error::CliError, config::coordinator::RejectionDiagnostic};

const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;

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
    fn parse(bytes: &[u8]) -> Result<Self, CliError> {
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
pub(crate) struct Response {
    pub(crate) status: u16,
    pub(crate) content_type: String,
    pub(crate) body: Vec<u8>,
}

impl Response {
    #[cfg(test)]
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, CliError> {
        Self::read(std::io::BufReader::new(std::io::Cursor::new(bytes)))
    }

    pub(crate) fn read(mut reader: impl BufRead) -> Result<Self, CliError> {
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

    pub(crate) fn into_result(self) -> Result<Self, CliError> {
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

    pub(crate) fn json(&self) -> Result<Value, CliError> {
        serde_json::from_slice(&self.body)
            .map_err(|_| CliError::Remote("endpoint returned invalid JSON".into()))
    }

    pub(crate) fn is_json(&self) -> bool {
        self.content_type.starts_with("application/json")
    }

    /// The body as the server wrote it when it is one JSON document; the check builds nothing.
    pub(crate) fn checked_json_body(&self) -> Result<&[u8], CliError> {
        serde_json::from_slice::<serde::de::IgnoredAny>(&self.body)
            .map_err(|_| CliError::Remote("endpoint returned invalid JSON".into()))?;
        Ok(self.body.trim_ascii_end())
    }

    pub(crate) fn output_value(&self) -> Result<Value, CliError> {
        if self.is_json() {
            self.json()
        } else {
            let text = std::str::from_utf8(&self.body)
                .map_err(|_| CliError::Remote("endpoint returned invalid UTF-8".into()))?;
            Ok(json!({ "contentType": self.content_type, "body": text }))
        }
    }
}
