use std::collections::HashSet;

use crate::{
    defaults::MAX_SAFE_JSON_INTEGER,
    http_validation::validate_content_type,
    model::{
        CacheCompressionCoding, ConfigError, HttpCacheCompression, MAX_CACHE_BROTLI_LEVEL,
        MAX_CACHE_COMPRESSION_CONTENT_TYPES, MAX_CACHE_GZIP_LEVEL, MAX_CACHE_ZSTD_LEVEL,
        MIN_CACHE_GZIP_LEVEL, MIN_CACHE_ZSTD_LEVEL,
    },
};

type Invalid<'a> = &'a dyn Fn(&'static str, String) -> ConfigError;

pub(crate) fn validate_cache_compression(
    compression: &HttpCacheCompression,
    invalid: Invalid<'_>,
) -> Result<(), ConfigError> {
    validate_codings(compression, invalid)?;
    validate_content_types(&compression.content_types, invalid)?;
    if compression.min_length_bytes > MAX_SAFE_JSON_INTEGER {
        return Err(invalid(
            "compression.min_length_bytes",
            "must not exceed the maximum safe JSON integer".into(),
        ));
    }
    validate_level(
        "compression.gzip_level",
        i16::from(compression.gzip_level),
        (MIN_CACHE_GZIP_LEVEL.into(), MAX_CACHE_GZIP_LEVEL.into()),
        invalid,
    )?;
    validate_level(
        "compression.brotli_level",
        i16::from(compression.brotli_level),
        (0, MAX_CACHE_BROTLI_LEVEL.into()),
        invalid,
    )?;
    validate_level(
        "compression.zstd_level",
        i16::from(compression.zstd_level),
        (MIN_CACHE_ZSTD_LEVEL.into(), MAX_CACHE_ZSTD_LEVEL.into()),
        invalid,
    )
}

fn validate_level(
    field: &'static str,
    level: i16,
    (min, max): (i16, i16),
    invalid: Invalid<'_>,
) -> Result<(), ConfigError> {
    if (min..=max).contains(&level) {
        return Ok(());
    }
    Err(invalid(field, format!("must be between {min} and {max}")))
}

fn validate_codings(
    compression: &HttpCacheCompression,
    invalid: Invalid<'_>,
) -> Result<(), ConfigError> {
    if compression.codings.is_empty() {
        return Err(invalid("compression.codings", "must not be empty".into()));
    }
    if !all_distinct(&compression.codings) {
        return Err(invalid(
            "compression.codings",
            "must not contain duplicates".into(),
        ));
    }
    if !all_distinct(&compression.precompress) {
        return Err(invalid(
            "compression.precompress",
            "must not contain duplicates".into(),
        ));
    }
    if !compression
        .precompress
        .iter()
        .all(|coding| compression.codings.contains(coding))
    {
        return Err(invalid(
            "compression.precompress",
            "must only list codings offered in `codings`".into(),
        ));
    }
    Ok(())
}

fn all_distinct(codings: &[CacheCompressionCoding]) -> bool {
    let mut seen = HashSet::with_capacity(codings.len());
    codings.iter().all(|coding| seen.insert(*coding))
}

fn validate_content_types(
    content_types: &[String],
    invalid: Invalid<'_>,
) -> Result<(), ConfigError> {
    let field = "compression.content_types";
    if content_types.is_empty() || content_types.len() > MAX_CACHE_COMPRESSION_CONTENT_TYPES {
        return Err(invalid(
            field,
            format!("must contain 1 through {MAX_CACHE_COMPRESSION_CONTENT_TYPES} values"),
        ));
    }
    let mut seen = HashSet::with_capacity(content_types.len());
    for content_type in content_types {
        validate_content_type(content_type).map_err(|detail| invalid(field, detail.into()))?;
        if !seen.insert(content_type.to_ascii_lowercase()) {
            return Err(invalid(field, "must not contain duplicates".into()));
        }
    }
    Ok(())
}
