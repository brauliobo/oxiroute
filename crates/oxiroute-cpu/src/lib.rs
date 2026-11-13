//! Runtime CPU dispatch for the hot byte scanners.
//!
//! The binary is built for generic x86-64; nothing here depends on a `target-cpu` or `target-feature` flag. [`level`]
//! detects the CPU once ([`Level`]) and every scanner picks the widest kernel that level allows: SSE4.2 (16 bytes),
//! AVX2 (32) or AVX-512 (64). The scalar scanner is the fallback of every level and the oracle of `tests/`.
//!
//! `OXIROUTE_CPU_LEVEL=scalar|sse42|avx2|avx512` lowers the level (to triage or to test a kernel on a CPU that has
//! more); it can never raise it above what the CPU runs. Off x86-64, or without the `std` feature, the level is
//! always [`Level::Scalar`].
//!
//! The `*_with` functions run a scanner at a given level, clamped to the hardware, for tests and benchmarks.

#![no_std]

#[cfg(feature = "std")]
extern crate std;

mod level;
mod scalar;
#[cfg(target_arch = "x86_64")]
mod simd;
#[cfg(not(target_arch = "x86_64"))]
mod simd {
    use crate::{Level, TokenRun, scalar};

    pub(crate) fn control_at(_: Level, buf: &[u8], i: usize) -> usize {
        scalar::control_at(buf, i)
    }

    pub(crate) fn token_run(_: Level, buf: &[u8]) -> TokenRun {
        scalar::token_run(buf)
    }
}

pub use level::{ENV, Level, level};

/// Whether `b` is a `tchar` of RFC 9110: a byte a header name or a method is made of.
#[must_use]
pub const fn is_tchar(b: u8) -> bool {
    matches!(b, b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' | b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+'
        | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~')
}

/// The run of token bytes at the start of a buffer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TokenRun {
    pub len: usize,
    /// The run has an ASCII capital.
    pub upper: bool,
    /// The run has an underscore.
    pub underscore: bool,
}

/// Index of the first byte below 0x20 or DEL at or after `i`, or the buffer length. A tab is reported like any other
/// control byte.
#[inline]
#[must_use]
pub fn control_at(buf: &[u8], i: usize) -> usize {
    simd::control_at(level(), buf, i)
}

/// [`control_at`] at `level`, clamped to the hardware.
#[must_use]
pub fn control_at_with(level: Level, buf: &[u8], i: usize) -> usize {
    simd::control_at(level.clamp(), buf, i)
}

/// The run of token bytes ([`is_tchar`]) at the start of `buf`.
#[inline]
#[must_use]
pub fn token_run(buf: &[u8]) -> TokenRun {
    simd::token_run(level(), buf)
}

/// [`token_run`] at `level`, clamped to the hardware.
#[must_use]
pub fn token_run_with(level: Level, buf: &[u8]) -> TokenRun {
    simd::token_run(level.clamp(), buf)
}
