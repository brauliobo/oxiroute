#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn parse_listen(value: &str) -> Option<SocketAddr> {
    if value.contains(',') {
        return None;
    }
    if let Some(port) = value.strip_prefix(':') {
        return port
            .parse()
            .ok()
            .map(|port| SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), port));
    }
    value.parse().ok()
}

pub(super) fn canonical_storage_path(value: &String) -> Option<PathBuf> {
    let path = std::path::Path::new(value);
    let text = value.as_bytes();
    (path.is_absolute()
        && !text.contains(&0)
        && !value.contains("//")
        && !value.ends_with('/')
        && !value
            .split('/')
            .any(|segment| matches!(segment, "." | "..")))
    .then(|| path.to_path_buf())
}

pub(super) fn canonical_unix_path(value: &str) -> Option<PathBuf> {
    let mut output = String::new();
    for segment in value.split('/').filter(|segment| !segment.is_empty()) {
        if matches!(segment, "." | "..") {
            return None;
        }
        output.push('/');
        output.push_str(segment);
    }
    (!output.is_empty() && output.len() <= 107).then(|| output.into())
}

pub(super) fn same_timing(left: RouteTiming, right: RouteTiming) -> bool {
    left.connect_ms == right.connect_ms
        && left.read_ms == right.read_ms
        && left.write_ms == right.write_ms
}

pub(super) fn expression_name(expression: &Expression) -> Option<&[u8]> {
    match &expression.kind {
        ExpressionKind::Name(value) => Some(&value.bytes),
        _ => None,
    }
}

pub(super) fn is_host_name(value: &[u8]) -> bool {
    value.eq_ignore_ascii_case(b"req.http.host")
}

pub(super) fn static_string(expression: &Expression) -> Option<String> {
    match &expression.kind {
        ExpressionKind::Literal(super::super::Literal::String(value)) => {
            String::from_utf8(value.bytes.clone()).ok()
        }
        _ => None,
    }
}

pub(super) fn static_text(expression: &Expression) -> Option<String> {
    match &expression.kind {
        ExpressionKind::Literal(
            super::super::Literal::String(value) | super::super::Literal::Number(value),
        ) => String::from_utf8(value.bytes.clone()).ok(),
        _ => None,
    }
}

pub(super) fn probe_provenance(expression: &Expression, fallback: &Provenance) -> Provenance {
    Provenance {
        span: expression.span,
        include_stack: fallback.include_stack.clone(),
    }
}

pub(super) fn static_port(expression: &Expression) -> Option<u16> {
    static_text(expression)?
        .parse()
        .ok()
        .filter(|port| *port > 0)
}

pub(super) fn integer(expression: &Expression) -> Option<u64> {
    static_text(expression)?.parse().ok()
}

pub(super) fn boolean(expression: &Expression) -> Option<bool> {
    match expression_name(expression) {
        Some(b"true") => Some(true),
        Some(b"false") => Some(false),
        _ => None,
    }
}

pub(super) fn duration_ms(expression: &Expression) -> Option<u64> {
    static_text(expression).and_then(|value| parse_duration_ms_bytes(&value))
}

pub(super) fn parse_duration_ms_bytes(value: &str) -> Option<u64> {
    let split = value.as_bytes().iter().position(u8::is_ascii_alphabetic)?;
    let number = &value[..split];
    let unit = value[split..].to_ascii_lowercase();
    let unit_nanos = match unit.as_str() {
        "ns" => 1_u128,
        "us" => 1_000,
        "ms" => 1_000_000,
        "s" => 1_000_000_000,
        "m" => 60 * 1_000_000_000,
        "h" => 60 * 60 * 1_000_000_000,
        "d" => 24 * 60 * 60 * 1_000_000_000,
        "w" => 7 * 24 * 60 * 60 * 1_000_000_000,
        _ => return None,
    };
    let (whole, fraction) = number
        .split_once('.')
        .map_or((number, ""), |(whole, fraction)| (whole, fraction));
    if whole.is_empty()
        || fraction.len() > 9
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let scale = 10_u128.checked_pow(u32::try_from(fraction.len()).ok()?)?;
    let fraction_value = if fraction.is_empty() {
        0
    } else {
        fraction.parse().ok()?
    };
    let numerator = whole
        .parse::<u128>()
        .ok()?
        .checked_mul(scale)?
        .checked_add(fraction_value)?;
    let nanos = numerator.checked_mul(unit_nanos)?;
    if nanos % scale != 0 || (nanos / scale) % 1_000_000 != 0 {
        return None;
    }
    u64::try_from(nanos / scale / 1_000_000).ok()
}

pub(super) fn parse_size(value: &str) -> Option<u64> {
    let split = value
        .as_bytes()
        .iter()
        .position(u8::is_ascii_alphabetic)
        .unwrap_or(value.len());
    let number = value[..split].parse::<u64>().ok()?;
    let multiplier = match value[split..].to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" => 1 << 10,
        "m" | "mb" => 1 << 20,
        "g" | "gb" => 1 << 30,
        "t" | "tb" => 1 << 40,
        "p" | "pb" => 1 << 50,
        _ => return None,
    };
    number.checked_mul(multiplier)
}

pub(super) fn request_mutation(mutation: &HeaderMutation) -> Option<HttpRequestHeaderMutation> {
    let name = header_name(&mutation.name)?;
    match mutation.operation {
        HeaderOperation::Set => Some(HttpRequestHeaderMutation::Set {
            name,
            value: HttpRequestHeaderValue::Literal {
                value: header_value(mutation.value.as_ref()?)?,
            },
        }),
        HeaderOperation::Remove => Some(HttpRequestHeaderMutation::Remove { name }),
        HeaderOperation::Append => None,
    }
}

pub(super) fn response_mutation(mutation: &HeaderMutation) -> Option<HttpResponseHeaderMutation> {
    let name = header_name(&mutation.name)?;
    match mutation.operation {
        HeaderOperation::Set => Some(HttpResponseHeaderMutation::Set {
            name,
            value: header_value(mutation.value.as_ref()?)?,
            always: true,
        }),
        HeaderOperation::Append => Some(HttpResponseHeaderMutation::Add {
            name,
            value: header_value(mutation.value.as_ref()?)?,
            always: true,
        }),
        HeaderOperation::Remove => Some(HttpResponseHeaderMutation::Remove { name }),
    }
}

fn header_name(value: &[u8]) -> Option<String> {
    let name = HeaderName::from_bytes(value).ok()?;
    Some(name.as_str().to_owned())
}

fn header_value(expression: &Expression) -> Option<String> {
    let value = static_text(expression)?;
    HeaderValue::from_str(&value).ok()?;
    Some(value)
}

pub(super) fn display(value: &[u8]) -> String {
    String::from_utf8(value.to_vec()).unwrap_or_else(|_| "<non-UTF-8>".into())
}
