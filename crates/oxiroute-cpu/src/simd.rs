//! The vector kernels, one per level, behind [`dispatch!`].
//!
//! Everything `unsafe` of the crate is in this file, of two kinds:
//!
//! * calling a `#[target_feature]` kernel from the dispatchers: the level they receive never exceeds the hardware
//!   level ([`Level::clamp`], or [`crate::level`] which is lowered from it), so the CPU has every feature the kernel
//!   of that level enables;
//! * loading a vector register: from a reference to an array (all bytes readable, unaligned loads) or, for AVX-512,
//!   masked so that no lane past the slice is read.
#![allow(unsafe_code)]

use crate::lanes::{Job, Lanes, WIDTH, run_scalar, sealed::Sealed};
use crate::{Level, is_target_byte, is_tchar, scalar};
use core::arch::x86_64::*;

/// Calls the arm of `$level`; the scalar arm is the answer for [`Level::Scalar`].
macro_rules! dispatch {
    ($level:expr, $scalar:expr, sse42: $sse42:expr, avx2: $avx2:expr, avx512: $avx512:expr) => {
        match $level {
            // SAFETY: see the module docs: `$level` is at most the hardware level.
            Level::Avx512 => unsafe { $avx512 },
            Level::Avx2 => unsafe { $avx2 },
            Level::Sse42 => unsafe { $sse42 },
            Level::Scalar => $scalar,
        }
    };
}

pub(crate) fn control_at(level: Level, buf: &[u8], i: usize) -> usize {
    dispatch!(level, scalar::control_at(buf, i),
        sse42: control_at_sse42(buf, i), avx2: control_at_avx2(buf, i), avx512: control_at_avx512(buf, i))
}

pub(crate) fn dispatch<J: Job>(level: Level, job: J) -> J::Out {
    dispatch!(level, run_scalar(job), sse42: run_sse42(job), avx2: run_avx2(job), avx512: run_avx2(job))
}

fn chunk<const N: usize>(buf: &[u8], i: usize) -> Option<&[u8; N]> {
    buf.get(i..)?.first_chunk()
}

/// The first `n` bits, `n <= 64`.
fn low_bits(n: u32) -> u64 {
    u64::MAX.checked_shr(64 - n).unwrap_or(0)
}

#[inline]
#[target_feature(enable = "sse2")]
fn load128(bytes: &[u8; 16]) -> __m128i {
    // SAFETY: 16 readable bytes, unaligned load.
    unsafe { _mm_loadu_si128(bytes.as_ptr().cast()) }
}

#[inline]
#[target_feature(enable = "avx2")]
fn load256(bytes: &[u8; 32]) -> __m256i {
    // SAFETY: 32 readable bytes, unaligned load.
    unsafe { _mm256_loadu_si256(bytes.as_ptr().cast()) }
}

#[inline]
#[target_feature(enable = "avx512bw")]
fn load512(bytes: &[u8; 64]) -> __m512i {
    // SAFETY: 64 readable bytes, unaligned load.
    unsafe { _mm512_loadu_si512(bytes.as_ptr().cast()) }
}

/// The first `live.count_ones()` bytes of `rest` (`live` is the low bits), zero elsewhere.
#[inline]
#[target_feature(enable = "avx512bw")]
fn load512_masked(rest: &[u8], live: u64) -> __m512i {
    // SAFETY: `live` has no bit at or past `rest.len()` and a masked-off lane is not read.
    unsafe { _mm512_maskz_loadu_epi8(live, rest.as_ptr().cast()) }
}

/// The live lanes of a 64-byte step: `rest.len()` of them, at most 64.
fn live(rest: &[u8]) -> u64 {
    low_bits(rest.len().min(64) as u32)
}

#[target_feature(enable = "sse4.2,popcnt")]
fn control_at_sse42(buf: &[u8], mut i: usize) -> usize {
    while let Some(bytes) = chunk(buf, i) {
        let x = load128(bytes);
        let ctl = _mm_cmpeq_epi8(_mm_min_epu8(x, _mm_set1_epi8(0x1f)), x);
        let found = _mm_movemask_epi8(_mm_or_si128(ctl, _mm_cmpeq_epi8(x, _mm_set1_epi8(0x7f))));
        if found != 0 {
            return i + found.trailing_zeros() as usize;
        }
        i += 16;
    }
    scalar::control_at(buf, i)
}

#[target_feature(enable = "avx2,bmi1,bmi2")]
fn control_at_avx2(buf: &[u8], mut i: usize) -> usize {
    while let Some(bytes) = chunk(buf, i) {
        let x = load256(bytes);
        let ctl = _mm256_cmpeq_epi8(_mm256_min_epu8(x, _mm256_set1_epi8(0x1f)), x);
        let found = _mm256_movemask_epi8(_mm256_or_si256(
            ctl,
            _mm256_cmpeq_epi8(x, _mm256_set1_epi8(0x7f)),
        ));
        if found != 0 {
            return i + found.trailing_zeros() as usize;
        }
        i += 32;
    }
    scalar::control_at(buf, i)
}

/// Lanes of `x` below 0x20 or DEL.
#[inline]
#[target_feature(enable = "avx512bw")]
fn controls512(x: __m512i) -> u64 {
    _mm512_cmple_epu8_mask(x, _mm512_set1_epi8(0x1f))
        | _mm512_cmpeq_epi8_mask(x, _mm512_set1_epi8(0x7f))
}

#[target_feature(enable = "avx512bw,bmi1,bmi2")]
fn control_at_avx512(buf: &[u8], mut i: usize) -> usize {
    while let Some(bytes) = chunk(buf, i) {
        let found = controls512(load512(bytes));
        if found != 0 {
            return i + found.trailing_zeros() as usize;
        }
        i += 64;
    }
    let Some(rest) = buf.get(i..).filter(|rest| !rest.is_empty()) else {
        return i;
    };
    let live = live(rest);
    let found = controls512(load512_masked(rest, live)) & live;
    if found != 0 {
        i + found.trailing_zeros() as usize
    } else {
        buf.len()
    }
}

/// Nibble tables of a byte set of the rows `0x2_` to `0x7_`: a byte is in the set when `LO[byte & 15] & HI[byte >> 4]`
/// is not zero. Each high nibble of `0x2_..=0x7_` owns one bit, which the low nibbles that make a member with it carry.
macro_rules! nibble_lo {
    ($member:path) => {{
        let mut table = [0; 16];
        let mut low = 0;
        while low < 16 {
            let mut row = 2;
            while row < 8 {
                table[low as usize] |= if $member(row << 4 | low) { 1 << (row - 2) } else { 0 };
                row += 1;
            }
            low += 1;
        }
        table
    }};
}

static TOKEN_LO: [u8; 16] = nibble_lo!(is_tchar);
static TARGET_LO: [u8; 16] = nibble_lo!(is_target_byte);
static ROW_BITS: [u8; 16] = [0, 0, 1, 2, 4, 8, 16, 32, 0, 0, 0, 0, 0, 0, 0, 0];

fn bits(mask: i32) -> u32 {
    mask.cast_unsigned()
}

#[inline]
#[target_feature(enable = "sse4.2")]
fn lookup128(x: __m128i, lo: &[u8; 16]) -> __m128i {
    let nibble = _mm_set1_epi8(0x0f);
    let low = _mm_shuffle_epi8(load128(lo), _mm_and_si128(x, nibble));
    let high = _mm_shuffle_epi8(load128(&ROW_BITS), _mm_and_si128(_mm_srli_epi16::<4>(x), nibble));
    _mm_cmpeq_epi8(_mm_and_si128(low, high), _mm_setzero_si128())
}

#[inline]
#[target_feature(enable = "avx2")]
fn lookup256(x: __m256i, lo: &[u8; 16]) -> __m256i {
    let nibble = _mm256_set1_epi8(0x0f);
    let table = |t: &[u8; 16]| _mm256_broadcastsi128_si256(load128(t));
    let low = _mm256_shuffle_epi8(table(lo), _mm256_and_si256(x, nibble));
    let high = _mm256_shuffle_epi8(
        table(&ROW_BITS),
        _mm256_and_si256(_mm256_srli_epi16::<4>(x), nibble),
    );
    _mm256_cmpeq_epi8(_mm256_and_si256(low, high), _mm256_setzero_si256())
}

#[target_feature(enable = "sse4.2,popcnt")]
fn run_sse42<J: Job>(job: J) -> J::Out {
    job.run(Sse42(()))
}

#[target_feature(enable = "avx2,bmi1,bmi2")]
fn run_avx2<J: Job>(job: J) -> J::Out {
    job.run(Avx2(()))
}

/// 16-lane registers, two per window.
#[derive(Clone, Copy)]
pub(crate) struct Sse42(());

/// 32-lane registers; the level above runs them too, as lines are shorter than 64 bytes.
#[derive(Clone, Copy)]
pub(crate) struct Avx2(());

impl Sealed for Sse42 {}
impl Sealed for Avx2 {}

macro_rules! pair_mask {
    ($pair:expr, $each:expr) => {{
        let (a, b) = $pair;
        bits(_mm_movemask_epi8($each(a))) | bits(_mm_movemask_epi8($each(b))) << 16
    }};
}

impl Lanes for Sse42 {
    type Reg = (__m128i, __m128i);

    #[inline(always)]
    fn load(self, bytes: &[u8; WIDTH]) -> Self::Reg {
        let (a, b) = bytes.split_at(16);
        // SAFETY: both halves are 16 readable bytes, unaligned loads; the level has SSE4.2, which includes SSE2.
        unsafe {
            (
                _mm_loadu_si128(a.as_ptr().cast()),
                _mm_loadu_si128(b.as_ptr().cast()),
            )
        }
    }

    #[inline(always)]
    fn token_stop(self, reg: Self::Reg) -> u32 {
        // SAFETY: a value of this type exists only at a level that has SSE4.2 (see the module docs).
        unsafe { pair_mask!(reg, |x| lookup128(x, &TOKEN_LO)) }
    }

    #[inline(always)]
    fn upper(self, reg: Self::Reg) -> u32 {
        // SAFETY: as above.
        unsafe {
            pair_mask!(reg, |x| {
                let rel = _mm_sub_epi8(x, _mm_set1_epi8(b'A'.cast_signed()));
                _mm_cmpeq_epi8(_mm_min_epu8(rel, _mm_set1_epi8(25)), rel)
            })
        }
    }

    #[inline(always)]
    fn underscore(self, reg: Self::Reg) -> u32 {
        // SAFETY: as above.
        unsafe { pair_mask!(reg, |x| _mm_cmpeq_epi8(x, _mm_set1_epi8(b'_'.cast_signed()))) }
    }

    #[inline(always)]
    fn control(self, reg: Self::Reg) -> u32 {
        // SAFETY: as above.
        unsafe {
            pair_mask!(reg, |x| {
                let below = _mm_cmpeq_epi8(_mm_min_epu8(x, _mm_set1_epi8(0x1f)), x);
                _mm_or_si128(below, _mm_cmpeq_epi8(x, _mm_set1_epi8(0x7f)))
            })
        }
    }

    #[inline(always)]
    fn target_stop(self, reg: Self::Reg) -> u32 {
        // SAFETY: as above.
        unsafe { pair_mask!(reg, |x| lookup128(x, &TARGET_LO)) }
    }
}

impl Lanes for Avx2 {
    type Reg = __m256i;

    #[inline(always)]
    fn load(self, bytes: &[u8; WIDTH]) -> Self::Reg {
        // SAFETY: 32 readable bytes, unaligned load; a value of this type exists only at a level that has AVX2.
        unsafe { _mm256_loadu_si256(bytes.as_ptr().cast()) }
    }

    #[inline(always)]
    fn token_stop(self, reg: Self::Reg) -> u32 {
        // SAFETY: a value of this type exists only at a level that has AVX2 (see the module docs).
        unsafe { bits(_mm256_movemask_epi8(lookup256(reg, &TOKEN_LO))) }
    }

    #[inline(always)]
    fn upper(self, reg: Self::Reg) -> u32 {
        // SAFETY: as above.
        unsafe {
            let rel = _mm256_sub_epi8(reg, _mm256_set1_epi8(b'A'.cast_signed()));
            bits(_mm256_movemask_epi8(_mm256_cmpeq_epi8(
                _mm256_min_epu8(rel, _mm256_set1_epi8(25)),
                rel,
            )))
        }
    }

    #[inline(always)]
    fn underscore(self, reg: Self::Reg) -> u32 {
        // SAFETY: as above.
        unsafe {
            bits(_mm256_movemask_epi8(_mm256_cmpeq_epi8(
                reg,
                _mm256_set1_epi8(b'_'.cast_signed()),
            )))
        }
    }

    #[inline(always)]
    fn control(self, reg: Self::Reg) -> u32 {
        // SAFETY: as above.
        unsafe {
            let below = _mm256_cmpeq_epi8(_mm256_min_epu8(reg, _mm256_set1_epi8(0x1f)), reg);
            let del = _mm256_cmpeq_epi8(reg, _mm256_set1_epi8(0x7f));
            bits(_mm256_movemask_epi8(_mm256_or_si256(below, del)))
        }
    }

    #[inline(always)]
    fn target_stop(self, reg: Self::Reg) -> u32 {
        // SAFETY: as above.
        unsafe { bits(_mm256_movemask_epi8(lookup256(reg, &TARGET_LO))) }
    }
}
