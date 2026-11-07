#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn read_record(
    root: &OwnedFd,
    name: &str,
    path_metadata: &rustix_fs::Stat,
    owner: u32,
    config: &DiskCacheConfig,
    cache: &Cache,
) -> Result<DecodedRecord, DiskCacheError> {
    if !secure_file(path_metadata, owner) {
        return Err(DiskCacheError::UnsafeEntry);
    }
    let descriptor = rustix_fs::openat(
        root,
        name,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(storage)?;
    let before = rustix_fs::fstat(&descriptor).map_err(storage)?;
    if before.st_dev != path_metadata.st_dev || before.st_ino != path_metadata.st_ino {
        return Err(DiskCacheError::UnsafeEntry);
    }
    let size = usize::try_from(before.st_size).map_err(|_| DiskCacheError::RecordTooLarge)?;
    if size > config.max_record_bytes {
        return Err(DiskCacheError::RecordTooLarge);
    }
    let mut file = File::from(descriptor);
    let mut bytes = Vec::with_capacity(size);
    Read::by_ref(&mut file)
        .take((config.max_record_bytes as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(storage)?;
    let after = rustix_fs::fstat(&file).map_err(storage)?;
    if bytes.len() != size
        || before.st_dev != after.st_dev
        || before.st_ino != after.st_ino
        || before.st_size != after.st_size
    {
        return Err(DiskCacheError::UnsafeEntry);
    }
    decode_record(&bytes, config, cache.now())
}

pub(super) fn encode_record(
    entry: &PreparedEntry,
    sequence: u64,
    access: u64,
    now: MonoTime,
    wall: SystemTime,
) -> Result<Vec<u8>, DiskCacheError> {
    let mut payload = Encoder::default();
    payload.u64(sequence);
    payload.u64(access);
    payload.duration(
        entry
            .policy
            .corrected_initial_age
            .saturating_add(now.saturating_duration_since(entry.response_received)),
    );
    payload.duration(wall.duration_since(UNIX_EPOCH).unwrap_or_default());
    payload.bytes(entry.key.base.method.as_str().as_bytes())?;
    payload.bytes(entry.key.base.scheme.as_bytes())?;
    payload.bytes(entry.key.base.authority.as_bytes())?;
    payload.bytes(entry.key.base.path.as_bytes())?;
    payload.optional_bytes(entry.key.base.query.as_deref().map(str::as_bytes))?;
    payload.u32_len(entry.key.vary.len())?;
    for vary in &entry.key.vary {
        payload.bytes(vary.name.as_str().as_bytes())?;
        payload.boolean(vary.present);
        payload.bytes(&vary.value)?;
    }
    payload.u16(entry.status.as_u16());
    payload.u32_len(entry.headers.len())?;
    for (name, value) in &entry.headers {
        payload.bytes(name.as_str().as_bytes())?;
        payload.bytes(value.as_bytes())?;
    }
    payload.bytes_u64(&entry.body)?;
    payload.u32_len(entry.tags.len())?;
    for tag in &entry.tags {
        payload.bytes(tag)?;
    }
    payload.duration(entry.policy.freshness_lifetime);
    payload.duration(entry.policy.stale_while_revalidate);
    payload.duration(entry.policy.stale_if_error);
    payload.optional_duration(entry.policy.retention.keep());
    payload.boolean(entry.policy.retention.request_stale_allowed());
    payload.boolean(entry.policy.always_revalidate);
    payload.boolean(entry.policy.must_revalidate_stale);
    payload.boolean(entry.policy.allows_authorized_reuse);
    payload.optional_bytes(entry.validators.etag.as_ref().map(HeaderValue::as_bytes))?;
    payload.optional_bytes(
        entry
            .validators
            .last_modified
            .as_ref()
            .map(HeaderValue::as_bytes),
    )?;
    payload.u64(u64::try_from(entry.charge).map_err(|_| DiskCacheError::RecordTooLarge)?);

    let payload_len = u64::try_from(payload.0.len()).map_err(|_| DiskCacheError::RecordTooLarge)?;
    let checksum = crc32fast::hash(&payload.0);
    let mut record = Vec::with_capacity(HEADER_BYTES.saturating_add(payload.0.len()));
    record.extend_from_slice(MAGIC);
    record.extend_from_slice(&RECORD_VERSION.to_le_bytes());
    record.extend_from_slice(&payload_len.to_le_bytes());
    record.extend_from_slice(&checksum.to_le_bytes());
    record.extend_from_slice(&payload.0);
    Ok(record)
}

#[allow(clippy::too_many_lines)]
fn decode_record(
    record: &[u8],
    config: &DiskCacheConfig,
    now: MonoTime,
) -> Result<DecodedRecord, DiskCacheError> {
    if record.len() < HEADER_BYTES || &record[..8] != MAGIC {
        return Err(invalid_record());
    }
    let version = u16::from_le_bytes(record[8..10].try_into().map_err(|_| invalid_record())?);
    let payload_len = u64::from_le_bytes(record[10..18].try_into().map_err(|_| invalid_record())?);
    let checksum = u32::from_le_bytes(record[18..22].try_into().map_err(|_| invalid_record())?);
    if !matches!(version, 1 | RECORD_VERSION)
        || usize::try_from(payload_len).ok() != Some(record.len().saturating_sub(HEADER_BYTES))
        || crc32fast::hash(&record[HEADER_BYTES..]) != checksum
    {
        return Err(invalid_record());
    }
    let mut input = Decoder::new(&record[HEADER_BYTES..]);
    let sequence = input.u64()?;
    let access = input.u64()?;
    let stored_age = input.duration()?;
    let stored_wall = UNIX_EPOCH
        .checked_add(input.duration()?)
        .ok_or_else(invalid_record)?;
    let downtime = SystemTime::now()
        .duration_since(stored_wall)
        .unwrap_or_default();
    let corrected_initial_age = stored_age.saturating_add(downtime);

    let method_bytes = input.bytes(config.memory.max_key_bytes)?;
    let method = Method::from_bytes(method_bytes).map_err(|_| invalid_record())?;
    let scheme = input.string(config.memory.max_key_bytes)?;
    let authority = input.string(config.memory.max_key_bytes)?;
    let path = input.string(config.memory.max_key_bytes)?;
    let query = input.optional_string(config.memory.max_key_bytes)?;
    let empty = HeaderMap::new();
    let canonical = BaseKey::new(
        RequestKeyInput {
            method: &method,
            scheme: &scheme,
            authority: &authority,
            path: &path,
            query: query.as_deref(),
            headers: &empty,
        },
        config.memory.max_key_bytes,
    )
    .map_err(|_| invalid_record())?;
    if canonical.method != method
        || canonical.scheme != scheme
        || canonical.authority != authority
        || canonical.path != path
        || canonical.query != query
        || !canonical.is_get()
    {
        return Err(invalid_record());
    }
    let vary_count = input.count(config.memory.max_vary_fields)?;
    let mut vary = Vec::with_capacity(vary_count);
    for _ in 0..vary_count {
        let name = HeaderName::from_bytes(input.bytes(config.memory.max_header_bytes)?)
            .map_err(|_| invalid_record())?;
        let present = input.boolean()?;
        let value = Bytes::copy_from_slice(input.bytes(config.memory.max_key_bytes)?);
        vary.push(VaryValue {
            name,
            present,
            value,
        });
    }
    if !vary
        .windows(2)
        .all(|pair| pair[0].name.as_str() < pair[1].name.as_str())
    {
        return Err(invalid_record());
    }
    let key = CacheKey {
        base: canonical,
        vary,
    };
    if key.encoded_len() > config.memory.max_key_bytes {
        return Err(invalid_record());
    }
    let status = StatusCode::from_u16(input.u16()?).map_err(|_| invalid_record())?;
    let header_count = input.count(config.memory.max_header_fields)?;
    let mut headers = HeaderMap::new();
    let mut header_bytes = 0usize;
    for _ in 0..header_count {
        let name_bytes = input.bytes(config.memory.max_header_bytes)?;
        let value_bytes = input.bytes(config.memory.max_header_bytes)?;
        header_bytes = header_bytes
            .checked_add(name_bytes.len())
            .and_then(|size| size.checked_add(value_bytes.len()))
            .ok_or_else(invalid_record)?;
        if header_bytes > config.memory.max_header_bytes {
            return Err(invalid_record());
        }
        let name = HeaderName::from_bytes(name_bytes).map_err(|_| invalid_record())?;
        let value = HeaderValue::from_bytes(value_bytes).map_err(|_| invalid_record())?;
        headers.append(name, value);
    }
    let body = Bytes::copy_from_slice(input.bytes_u64(config.memory.max_body_bytes)?);
    let tag_count = input.count(config.memory.max_tags_per_entry)?;
    let mut tags = Vec::with_capacity(tag_count);
    for _ in 0..tag_count {
        let tag = Bytes::copy_from_slice(input.bytes(config.memory.max_tag_bytes)?);
        validate_tag(&tag, config.memory.max_tag_bytes)?;
        if tags.contains(&tag) {
            return Err(invalid_record());
        }
        tags.push(tag);
    }
    let freshness_lifetime = input.duration()?;
    let stale_while_revalidate = input.duration()?;
    let stale_if_error = input.duration()?;
    let (retention_after_freshness, request_stale_allowed) = if version >= 2 {
        (input.optional_duration()?, input.boolean()?)
    } else {
        (None, true)
    };
    let retention = match (retention_after_freshness, request_stale_allowed) {
        (None, true) => RetentionPolicy::Rfc,
        (Some(keep), false) => RetentionPolicy::Canonical { keep },
        _ => return Err(invalid_record()),
    };
    let policy = ResponsePolicy {
        freshness_lifetime,
        corrected_initial_age,
        stale_while_revalidate,
        stale_if_error,
        retention,
        always_revalidate: input.boolean()?,
        must_revalidate_stale: input.boolean()?,
        allows_authorized_reuse: input.boolean()?,
    };
    let validators = Validators {
        etag: input
            .optional_bytes(config.memory.max_header_bytes)?
            .map(HeaderValue::from_bytes)
            .transpose()
            .map_err(|_| invalid_record())?,
        last_modified: input
            .optional_bytes(config.memory.max_header_bytes)?
            .map(HeaderValue::from_bytes)
            .transpose()
            .map_err(|_| invalid_record())?,
    };
    let stored_charge = usize::try_from(input.u64()?).map_err(|_| invalid_record())?;
    if !input.finished() {
        return Err(invalid_record());
    }
    let charge = object_charge(&key, &headers, body.len(), &tags).ok_or_else(invalid_record)?;
    if charge != stored_charge
        || charge > config.memory.max_object_bytes
        || charge > config.memory.max_total_bytes
    {
        return Err(invalid_record());
    }
    Ok(DecodedRecord {
        entry: RecoveredEntry {
            key,
            status,
            headers,
            body,
            tags,
            policy,
            validators,
            response_received: now,
            charge,
        },
        access,
        sequence,
    })
}
