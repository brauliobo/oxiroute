#[allow(clippy::wildcard_imports)]
use super::*;

/// Hard limits for all cache-owned allocations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheConfig {
    pub max_entries: usize,
    pub max_total_bytes: usize,
    pub max_object_bytes: usize,
    pub max_header_bytes: usize,
    pub max_header_fields: usize,
    pub max_body_bytes: usize,
    pub max_key_bytes: usize,
    pub max_vary_fields: usize,
    pub max_tags_per_entry: usize,
    pub max_tag_bytes: usize,
    pub max_in_flight: usize,
    pub max_followers_per_fill: usize,
    pub max_heuristic_freshness: Duration,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_entries: 10_000,
            max_total_bytes: 256 * 1024 * 1024,
            max_object_bytes: 8 * 1024 * 1024,
            max_header_bytes: 64 * 1024,
            max_header_fields: 256,
            max_body_bytes: 8 * 1024 * 1024,
            max_key_bytes: 16 * 1024,
            max_vary_fields: 32,
            max_tags_per_entry: 16,
            max_tag_bytes: 256,
            max_in_flight: 1_024,
            max_followers_per_fill: 1_024,
            max_heuristic_freshness: Duration::from_hours(24),
        }
    }
}

pub(super) fn validate_config(config: &CacheConfig) -> Result<(), CacheError> {
    let nonzero = config.max_entries > 0
        && config.max_total_bytes > 0
        && config.max_object_bytes > 0
        && config.max_header_bytes > 0
        && config.max_header_fields > 0
        && config.max_body_bytes > 0
        && config.max_key_bytes > 0
        && config.max_vary_fields > 0
        && config.max_tags_per_entry > 0
        && config.max_tag_bytes > 0
        && config.max_in_flight > 0
        && config.max_followers_per_fill > 0;
    if !nonzero
        || config.max_object_bytes > config.max_total_bytes
        || config.max_body_bytes > config.max_object_bytes
    {
        return Err(CacheError::InvalidConfig);
    }
    Ok(())
}

pub(super) fn check_header_bounds(
    headers: &HeaderMap,
    config: &CacheConfig,
) -> Result<(), CacheError> {
    if headers.len() > config.max_header_fields {
        return Err(CacheError::HeadersTooLarge);
    }
    let bytes = headers.iter().try_fold(0usize, |size, (name, value)| {
        size.checked_add(name.as_str().len())?
            .checked_add(value.len())
    });
    if bytes.is_none_or(|bytes| bytes > config.max_header_bytes) {
        return Err(CacheError::HeadersTooLarge);
    }
    Ok(())
}

pub(super) fn parse_tags(tags: &[&[u8]], config: &CacheConfig) -> Result<Vec<Bytes>, CacheError> {
    if tags.len() > config.max_tags_per_entry {
        return Err(CacheError::TooManyTags);
    }
    let mut parsed = Vec::with_capacity(tags.len());
    for tag in tags {
        validate_tag(tag, config.max_tag_bytes)?;
        let tag = Bytes::copy_from_slice(tag);
        if !parsed.contains(&tag) {
            parsed.push(tag);
        }
    }
    Ok(parsed)
}

pub(crate) fn validate_tag(tag: &[u8], max_bytes: usize) -> Result<(), CacheError> {
    if tag.is_empty()
        || tag.len() > max_bytes
        || !tag
            .iter()
            .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b',' | b'"'))
    {
        return Err(CacheError::InvalidTag);
    }
    Ok(())
}

pub(crate) fn object_charge(
    key: &CacheKey,
    headers: &HeaderMap,
    body_bytes: usize,
    tags: &[Bytes],
) -> Option<usize> {
    let mut charge = ENTRY_ACCOUNTING_OVERHEAD.checked_add(key.encoded_len())?;
    for (name, value) in headers {
        charge = charge
            .checked_add(HEADER_ACCOUNTING_OVERHEAD)?
            .checked_add(name.as_str().len())?
            .checked_add(value.len())?;
    }
    charge = charge.checked_add(body_bytes)?;
    for tag in tags {
        charge = charge
            .checked_add(TAG_ACCOUNTING_OVERHEAD)?
            .checked_add(tag.len())?;
    }
    Some(charge)
}
