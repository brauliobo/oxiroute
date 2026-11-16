use http::{HeaderMap, header::ACCEPT_ENCODING};

use crate::Coding;

const FULL_WEIGHT: u16 = 1000;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AcceptEncoding {
    weights: [Option<u16>; Coding::ALL.len()],
    wildcard: Option<u16>,
    identity: Option<u16>,
}

impl AcceptEncoding {
    #[must_use]
    pub fn from_headers(headers: &HeaderMap) -> Self {
        Self::parse(
            headers
                .get_all(ACCEPT_ENCODING)
                .iter()
                .map(http::HeaderValue::as_bytes),
        )
    }

    pub fn parse<'a>(lines: impl IntoIterator<Item = &'a [u8]>) -> Self {
        let mut accepted = Self::default();
        for element in lines
            .into_iter()
            .flat_map(|line| line.split(|byte| *byte == b','))
        {
            if let Some((token, weight)) = parse_element(element) {
                accepted.record(token, weight);
            }
        }
        accepted
    }

    fn record(&mut self, token: &[u8], weight: u16) {
        let slot = match token {
            b"*" => &mut self.wildcard,
            _ if token.eq_ignore_ascii_case(b"identity") => &mut self.identity,
            _ => match Coding::from_token(token) {
                Some(coding) => &mut self.weights[coding.index()],
                None => return,
            },
        };
        slot.get_or_insert(weight);
    }

    fn weight(&self, coding: Coding) -> u16 {
        self.weights[coding.index()].or(self.wildcard).unwrap_or(0)
    }

    fn identity_weight(&self) -> u16 {
        self.identity.or(self.wildcard).unwrap_or(0)
    }

    #[must_use]
    pub fn select(&self, supported: &[Coding]) -> Option<Coding> {
        let identity = self.identity_weight();
        supported
            .iter()
            .rev()
            .map(|coding| (self.weight(*coding), *coding))
            .filter(|(weight, _)| *weight > 0 && *weight >= identity)
            .max_by_key(|(weight, _)| *weight)
            .map(|(_, coding)| coding)
    }
}

fn parse_element(element: &[u8]) -> Option<(&[u8], u16)> {
    let mut parts = element.split(|byte| *byte == b';').map(trim);
    let token = parts.next().filter(|token| !token.is_empty())?;
    let mut weight = FULL_WEIGHT;
    for parameter in parts {
        let (name, value) = split_parameter(parameter)?;
        if name.eq_ignore_ascii_case(b"q") {
            weight = parse_weight(value)?;
        }
    }
    Some((token, weight))
}

fn split_parameter(parameter: &[u8]) -> Option<(&[u8], &[u8])> {
    let position = parameter.iter().position(|byte| *byte == b'=')?;
    Some((
        trim(&parameter[..position]),
        trim(&parameter[position + 1..]),
    ))
}

fn parse_weight(value: &[u8]) -> Option<u16> {
    let (whole, fraction) = match value.iter().position(|byte| *byte == b'.') {
        Some(dot) => (&value[..dot], &value[dot + 1..]),
        None => (value, &[][..]),
    };
    let whole = match whole {
        b"0" => 0,
        b"1" => 1,
        _ => return None,
    };
    if fraction.len() > 3 || !fraction.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let thousandths = fraction
        .iter()
        .chain(std::iter::repeat(&b'0'))
        .take(3)
        .fold(0u16, |sum, digit| sum * 10 + u16::from(digit - b'0'));
    (whole * FULL_WEIGHT + thousandths <= FULL_WEIGHT).then_some(whole * FULL_WEIGHT + thousandths)
}

fn trim(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !matches!(byte, b' ' | b'\t'))
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !matches!(byte, b' ' | b'\t'))
        .map_or(start, |index| index + 1);
    &bytes[start..end]
}
