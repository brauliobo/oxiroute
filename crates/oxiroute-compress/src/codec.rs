use crate::Coding;

pub const DEFAULT_GZIP_LEVEL: u8 = 6;
pub const DEFAULT_BROTLI_LEVEL: u8 = 5;
pub const DEFAULT_ZSTD_LEVEL: i8 = 7;
const MIN_ZSTD_LEVEL: i8 = -7;
const MAX_ZSTD_LEVEL: i8 = 19;
const MAX_BROTLI_LEVEL: u8 = 11;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Levels {
    pub gzip: u8,
    pub brotli: u8,
    pub zstd: i8,
}

impl Default for Levels {
    fn default() -> Self {
        Self {
            gzip: DEFAULT_GZIP_LEVEL,
            brotli: DEFAULT_BROTLI_LEVEL,
            zstd: DEFAULT_ZSTD_LEVEL,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CompressError {
    #[error("compression level is outside the codec range")]
    Level,
    #[error("the codec failed to compress the body")]
    Codec(#[from] std::io::Error),
}

/// # Errors
///
/// Returns an error when the level is outside the codec range or the codec fails.
pub fn compress(coding: Coding, levels: Levels, body: &[u8]) -> Result<Vec<u8>, CompressError> {
    match coding {
        Coding::Gzip => gzip(levels.gzip, body),
        Coding::Brotli => brotli(levels.brotli, body),
        Coding::Zstd => zstd(levels.zstd, body),
    }
}

fn zstd(level: i8, body: &[u8]) -> Result<Vec<u8>, CompressError> {
    if !(MIN_ZSTD_LEVEL..=MAX_ZSTD_LEVEL).contains(&level) {
        return Err(CompressError::Level);
    }
    Ok(zstd::bulk::compress(body, i32::from(level))?)
}

fn gzip(level: u8, body: &[u8]) -> Result<Vec<u8>, CompressError> {
    let level =
        libdeflater::CompressionLvl::new(i32::from(level)).map_err(|_| CompressError::Level)?;
    let mut compressor = libdeflater::Compressor::new(level);
    let mut out = vec![0; compressor.gzip_compress_bound(body.len())];
    let written = compressor
        .gzip_compress(body, &mut out)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    out.truncate(written);
    Ok(out)
}

fn brotli(quality: u8, body: &[u8]) -> Result<Vec<u8>, CompressError> {
    if quality > MAX_BROTLI_LEVEL {
        return Err(CompressError::Level);
    }
    let params = brotli::enc::BrotliEncoderParams {
        quality: i32::from(quality),
        lgwin: window_bits(body.len()),
        size_hint: body.len(),
        ..Default::default()
    };
    let mut out = Vec::new();
    brotli::BrotliCompress(&mut &body[..], &mut out, &params)?;
    Ok(out)
}

fn window_bits(length: usize) -> i32 {
    let needed = length.saturating_sub(1).bit_width();
    i32::try_from(needed).map_or(22, |bits| bits.clamp(10, 22))
}
