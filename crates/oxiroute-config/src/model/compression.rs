#[allow(clippy::wildcard_imports)]
use super::*;

pub const MIN_CACHE_ZSTD_LEVEL: i8 = -7;
pub const MAX_CACHE_ZSTD_LEVEL: i8 = 19;
pub const MAX_CACHE_BROTLI_LEVEL: u8 = 11;
pub const MIN_CACHE_GZIP_LEVEL: u8 = 1;
pub const MAX_CACHE_GZIP_LEVEL: u8 = 12;
pub const MAX_CACHE_COMPRESSION_CONTENT_TYPES: usize = 64;

/// Content-coded variants of cached bodies: each is compressed once, on the first request that negotiates it or at
/// fill time, and stored with the entry within the cache byte bound.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpCacheCompression {
    /// Offered codings, most preferred first; equal client weights resolve in this order.
    #[serde(default = "default_cache_compression_codings")]
    pub codings: Vec<CacheCompressionCoding>,
    /// Codings compressed when the response is stored; each must also be listed in `codings`.
    #[serde(default)]
    pub precompress: Vec<CacheCompressionCoding>,
    #[serde(default = "default_cache_compression_content_types")]
    pub content_types: Vec<String>,
    #[serde(default = "default_cache_compression_min_length_bytes")]
    pub min_length_bytes: u64,
    #[serde(default = "default_cache_compression_gzip_level")]
    pub gzip_level: u8,
    #[serde(default = "default_cache_compression_brotli_level")]
    pub brotli_level: u8,
    #[serde(default = "default_cache_compression_zstd_level")]
    pub zstd_level: i8,
}

impl Default for HttpCacheCompression {
    fn default() -> Self {
        Self {
            codings: default_cache_compression_codings(),
            precompress: Vec::new(),
            content_types: default_cache_compression_content_types(),
            min_length_bytes: default_cache_compression_min_length_bytes(),
            gzip_level: default_cache_compression_gzip_level(),
            brotli_level: default_cache_compression_brotli_level(),
            zstd_level: default_cache_compression_zstd_level(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CacheCompressionCoding {
    Zstd,
    #[serde(rename = "br")]
    Brotli,
    Gzip,
}

impl CacheCompressionCoding {
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Zstd => "zstd",
            Self::Brotli => "br",
            Self::Gzip => "gzip",
        }
    }
}

fn default_cache_compression_codings() -> Vec<CacheCompressionCoding> {
    vec![
        CacheCompressionCoding::Zstd,
        CacheCompressionCoding::Brotli,
        CacheCompressionCoding::Gzip,
    ]
}

fn default_cache_compression_content_types() -> Vec<String> {
    [
        "text/html",
        "text/plain",
        "text/css",
        "text/javascript",
        "application/javascript",
        "application/json",
        "application/xml",
        "image/svg+xml",
    ]
    .map(String::from)
    .to_vec()
}

const fn default_cache_compression_min_length_bytes() -> u64 {
    256
}

const fn default_cache_compression_gzip_level() -> u8 {
    6
}

const fn default_cache_compression_brotli_level() -> u8 {
    5
}

const fn default_cache_compression_zstd_level() -> i8 {
    7
}
