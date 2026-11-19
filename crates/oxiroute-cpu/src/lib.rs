//! Runtime CPU dispatch for the hot byte scanners.
//!
//! The binary is built for generic x86-64; nothing here depends on a `target-cpu` or `target-feature` flag. [`level`]
//! detects the CPU once ([`Level`]) and every scanner picks a supported kernel: SSE2/SSSE3 (16 bytes), AVX2 (32) or
//! AVX-512 BW (64). Scalar implementations are the oracle of `tests/` and handle the non-vector tails.
//!
//! [`control_at`] is a kernel of its own. A scanner of lines is a [`Job`] that [`dispatch`] runs with the [`Lanes`] of the
//! level, so that all of it is compiled for the level and every window of 32 bytes it reads inlines into it; the
//! AVX-512 level runs the AVX2 lanes, as lines are shorter than 64 bytes, and the SSE2 level runs the scalar lanes.
//!
//! [`init`] sets the level at process start from the `cpu_level` configuration option. Without a configured level,
//! `OXIROUTE_CPU_LEVEL=scalar|sse2|sse42|avx2|avx512` caps it (to triage or to test a kernel on a CPU that has
//! more); neither can raise it above what the CPU runs. Off x86-64, or without the `std` feature, the level is
//! always [`Level::Scalar`].
//!
//! The `*_with` functions and [`dispatch_with`] run a scanner at a given level, clamped to the hardware, for tests and
//! benchmarks.

#![no_std]

#[cfg(feature = "std")]
extern crate std;

mod lanes;
mod level;
mod scalar;
mod scan;
#[cfg(target_arch = "x86_64")]
mod simd;
#[cfg(not(target_arch = "x86_64"))]
mod simd {
    use crate::{Level, scalar};

    pub(crate) fn control_at(_: Level, buf: &[u8], i: usize) -> usize {
        scalar::control_at(buf, i)
    }

    pub(crate) fn dispatch<J: crate::Job>(_: Level, job: J) -> J::Out {
        crate::lanes::run_scalar(job)
    }
}

pub use lanes::{Job, Lanes, TokenMasks, WIDTH, Window, dispatch, dispatch_with};
pub use level::{ENV, Level, level};
#[cfg(feature = "std")]
pub use level::{InvalidEnv, init};
pub use scan::{
    ascii_range, ascii_range_with, find_any3, find_any3_with, header_value_end,
    header_value_end_with, json_escape_at, json_escape_at_with,
};

/// Whether `b` is a byte of an origin-form request target that `http::Uri` stores and writes back unchanged: stricter than
/// httparse on purpose, so no `"`, `` ` ``, `<`, `>`, `#`, space, control or high byte.
#[must_use]
pub const fn is_target_byte(b: u8) -> bool {
    matches!(b, 0x21 | 0x24..=0x3b | 0x3d | 0x3f..=0x5f | 0x61..=0x7e)
}

/// Whether `b` is a `tchar` of RFC 9110: a byte a header name or a method is made of.
#[must_use]
pub const fn is_tchar(b: u8) -> bool {
    matches!(b, b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' | b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+'
        | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~')
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
