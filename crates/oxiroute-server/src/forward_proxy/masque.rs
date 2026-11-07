#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) const CAPSULE_PROTOCOL: http::HeaderName =
    http::HeaderName::from_static("capsule-protocol");

const CONNECT_UDP_PATH: &str = "/.well-known/masque/udp";
const CONNECT_UDP_PATH_PREFIX: &str = "/.well-known/masque/udp/";

pub(super) fn classify_connect_udp_request<B>(request: &Request<B>) -> ConnectUdpRequestKind {
    let path = request.uri().path();
    let path_attempt = path == CONNECT_UDP_PATH || path.starts_with(CONNECT_UDP_PATH_PREFIX);
    let upgrade_attempt = header_has_token(request.headers(), &header::UPGRADE, "connect-udp");
    if !path_attempt && !upgrade_attempt {
        return ConnectUdpRequestKind::NotAttempt;
    }
    if request.method() != Method::GET
        || !header_has_token(request.headers(), &header::CONNECTION, "upgrade")
    {
        return ConnectUdpRequestKind::Malformed;
    }
    let mut upgrade = request.headers().get_all(header::UPGRADE).iter();
    let Some(upgrade_value) = upgrade.next() else {
        return ConnectUdpRequestKind::Malformed;
    };
    if upgrade.next().is_some()
        || !upgrade_value
            .to_str()
            .ok()
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("connect-udp"))
    {
        return ConnectUdpRequestKind::Malformed;
    }
    let mut hosts = request.headers().get_all(header::HOST).iter();
    let Some(host) = hosts.next() else {
        return ConnectUdpRequestKind::Malformed;
    };
    if hosts.next().is_some() || host.as_bytes().is_empty() {
        return ConnectUdpRequestKind::Malformed;
    }
    if !capsule_protocol_is_true(request.headers())
        || capsule_request_has_content(request.headers())
    {
        return ConnectUdpRequestKind::Malformed;
    }
    ConnectUdpRequestKind::Valid
}

fn capsule_protocol_is_true(headers: &HeaderMap) -> bool {
    let mut values = headers.get_all(&CAPSULE_PROTOCOL).iter();
    let Some(value) = values.next() else {
        return true;
    };
    values.next().is_none() && StructuredFieldParser::new(value.as_bytes()).is_true()
}

fn capsule_request_has_content(headers: &HeaderMap) -> bool {
    headers.contains_key(header::CONTENT_LENGTH)
        || headers.contains_key(header::CONTENT_TYPE)
        || headers.contains_key(header::TRANSFER_ENCODING)
        || headers.contains_key(header::TRAILER)
        || headers.contains_key(header::EXPECT)
}

struct StructuredFieldParser<'a> {
    input: &'a [u8],
    offset: usize,
}

impl<'a> StructuredFieldParser<'a> {
    const fn new(input: &'a [u8]) -> Self {
        Self { input, offset: 0 }
    }

    // This parser is a one-shot value object; consuming it prevents reuse after advancing its cursor.
    #[allow(clippy::wrong_self_convention)]
    fn is_true(mut self) -> bool {
        self.skip_spaces();
        if self.parse_boolean() != Some(true) || !self.parse_parameters() {
            return false;
        }
        self.skip_spaces();
        self.offset == self.input.len()
    }

    fn parse_parameters(&mut self) -> bool {
        while self.consume(b';') {
            self.skip_spaces();
            if !self.parse_key() {
                return false;
            }
            if self.consume(b'=') && !self.parse_bare_item() {
                return false;
            }
        }
        true
    }

    fn parse_key(&mut self) -> bool {
        let Some(first) = self.peek() else {
            return false;
        };
        if !(first.is_ascii_lowercase() || first == b'*') {
            return false;
        }
        self.offset += 1;
        while let Some(byte) = self.peek() {
            if byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'_' | b'-' | b'.' | b'*')
            {
                self.offset += 1;
            } else {
                break;
            }
        }
        true
    }

    fn parse_bare_item(&mut self) -> bool {
        match self.peek() {
            Some(b'-' | b'0'..=b'9') => self.parse_number(),
            Some(b'"') => self.parse_string(),
            Some(byte) if byte.is_ascii_alphabetic() || byte == b'*' => self.parse_token(),
            Some(b':') => self.parse_byte_sequence(),
            Some(b'?') => self.parse_boolean().is_some(),
            _ => false,
        }
    }

    fn parse_number(&mut self) -> bool {
        self.consume(b'-');
        let integer_start = self.offset;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.offset += 1;
        }
        let integer_digits = self.offset - integer_start;
        if integer_digits == 0 {
            return false;
        }
        if self.consume(b'.') {
            let fraction_start = self.offset;
            while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                self.offset += 1;
            }
            let fraction_digits = self.offset - fraction_start;
            integer_digits <= 12 && (1..=3).contains(&fraction_digits)
        } else {
            integer_digits <= 15
        }
    }

    fn parse_string(&mut self) -> bool {
        if !self.consume(b'"') {
            return false;
        }
        loop {
            let Some(byte) = self.take() else {
                return false;
            };
            match byte {
                b'"' => return true,
                b'\\' => {
                    if !matches!(self.take(), Some(b'"' | b'\\')) {
                        return false;
                    }
                }
                0x20..=0x7e => {}
                _ => return false,
            }
        }
    }

    fn parse_token(&mut self) -> bool {
        let Some(first) = self.take() else {
            return false;
        };
        if !(first.is_ascii_alphabetic() || first == b'*') {
            return false;
        }
        while self.peek().is_some_and(is_structured_token_byte) {
            self.offset += 1;
        }
        true
    }

    fn parse_byte_sequence(&mut self) -> bool {
        if !self.consume(b':') {
            return false;
        }
        let start = self.offset;
        while let Some(byte) = self.peek() {
            if byte == b':' {
                let encoded = &self.input[start..self.offset];
                let padding = encoded
                    .iter()
                    .rev()
                    .take_while(|byte| **byte == b'=')
                    .count();
                let valid_padding = padding <= 2
                    && encoded.len().is_multiple_of(4)
                    && encoded[..encoded.len().saturating_sub(padding)]
                        .iter()
                        .all(|byte| *byte != b'=');
                self.offset += 1;
                return valid_padding;
            }
            if !is_structured_base64_byte(byte) {
                return false;
            }
            self.offset += 1;
        }
        false
    }

    fn parse_boolean(&mut self) -> Option<bool> {
        if !self.consume(b'?') {
            return None;
        }
        match self.take()? {
            b'0' => Some(false),
            b'1' => Some(true),
            _ => None,
        }
    }

    fn skip_spaces(&mut self) {
        while self.peek() == Some(b' ') {
            self.offset += 1;
        }
    }

    fn consume(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.offset += 1;
            true
        } else {
            false
        }
    }

    fn take(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.offset += 1;
        Some(byte)
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.offset).copied()
    }
}

fn is_structured_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
                | b':'
                | b'/'
        )
}

fn is_structured_base64_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=')
}
