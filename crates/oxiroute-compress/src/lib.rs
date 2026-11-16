mod codec;
mod coding;
mod negotiate;

pub use codec::{
    CompressError, DEFAULT_BROTLI_LEVEL, DEFAULT_GZIP_LEVEL, DEFAULT_ZSTD_LEVEL, Levels, compress,
};
pub use coding::Coding;
pub use negotiate::AcceptEncoding;

#[cfg(test)]
mod tests;
