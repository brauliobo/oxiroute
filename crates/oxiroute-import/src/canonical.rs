use std::{net::IpAddr, path::PathBuf, time::Duration};

use oxiroute_config::{canonical_dns_name, normalize_unix_path, validate_file_path};

pub(crate) fn duration_milliseconds(duration: Duration) -> Option<u64> {
    let milliseconds = u64::try_from(duration.as_millis()).ok()?;
    (milliseconds != 0 && Duration::from_millis(milliseconds) == duration).then_some(milliseconds)
}

pub(crate) fn ip_address(value: &[u8]) -> Option<IpAddr> {
    let value = value
        .strip_prefix(b"[")
        .and_then(|value| value.strip_suffix(b"]"))
        .unwrap_or(value);
    std::str::from_utf8(value).ok()?.parse().ok()
}

pub(crate) fn dns_name(value: &[u8]) -> Option<String> {
    canonical_dns_name(std::str::from_utf8(value).ok()?).ok()
}

pub(crate) fn absolute_file_path(value: &[u8]) -> Option<PathBuf> {
    let value = std::str::from_utf8(value).ok()?;
    let path = std::path::Path::new(value);
    validate_file_path(path).ok().map(|()| path.to_path_buf())
}

pub(crate) fn unix_socket_path(value: &[u8]) -> Option<PathBuf> {
    let mut path = PathBuf::from(std::str::from_utf8(value).ok()?);
    normalize_unix_path(&mut path).ok()?;
    Some(path)
}
