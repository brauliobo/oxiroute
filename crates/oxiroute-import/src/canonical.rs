use std::{net::IpAddr, path::PathBuf};

pub(crate) fn ip_address(value: &[u8]) -> Option<IpAddr> {
    let value = value
        .strip_prefix(b"[")
        .and_then(|value| value.strip_suffix(b"]"))
        .unwrap_or(value);
    std::str::from_utf8(value).ok()?.parse().ok()
}

pub(crate) fn dns_name(value: &[u8]) -> Option<String> {
    let mut name = std::str::from_utf8(value).ok()?.to_owned();
    if name.is_empty()
        || name.len() > 253
        || name.ends_with('.')
        || name.contains('*')
        || name.parse::<IpAddr>().is_ok()
        || !name.split('.').all(dns_label)
    {
        return None;
    }
    name.make_ascii_lowercase();
    Some(name)
}

fn dns_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 63
        && label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && label
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && label
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
}

pub(crate) fn unix_socket_path(value: &[u8]) -> Option<PathBuf> {
    let value = std::str::from_utf8(value).ok()?;
    if !value.starts_with('/') || value.as_bytes().contains(&0) || value.ends_with('/') {
        return None;
    }
    let mut normalized = String::with_capacity(value.len());
    for segment in value.split('/').filter(|segment| !segment.is_empty()) {
        if segment == "." || segment == ".." {
            return None;
        }
        normalized.push('/');
        normalized.push_str(segment);
    }
    (!normalized.is_empty() && normalized.len() <= 107).then(|| normalized.into())
}
