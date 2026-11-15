//! 32-lane windows over a byte slice, for scanners that read one line at a time.
//!
//! A scanner is a [`Job`]: [`dispatch`] resolves the CPU level once and runs the job with the [`Lanes`] of that level,
//! so the whole scan is compiled for the instruction set and every window operation inlines into it. A window is the
//! 32 bytes at an offset; each operation answers one bit per lane. Lanes at or past the end of the slice are never a
//! token or target byte and always a control byte, so a scan stops at the end of the slice without a tail loop.

use crate::{Level, is_target_byte, is_tchar, level, simd};

pub(crate) mod sealed {
    pub trait Sealed {}
}

/// Lanes of one window.
pub const WIDTH: usize = 32;

/// The answers of a window for a header name or a method.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TokenMasks {
    /// Lanes that are not a `tchar`, and the lanes past the end of the slice.
    pub stop: u32,
    /// Lanes that are an ASCII capital.
    pub upper: u32,
    /// Lanes that are an underscore.
    pub underscore: u32,
}

/// 32 bytes loaded at an offset of a slice.
#[derive(Clone, Copy)]
pub struct Window<R> {
    reg: R,
    shift: u32,
    past_end: u32,
}

/// The operations of one instruction-set level. Only [`dispatch`] hands one out, at a level the CPU runs.
pub trait Lanes: Copy + sealed::Sealed {
    type Reg: Copy;

    #[doc(hidden)]
    fn load(self, bytes: &[u8; WIDTH]) -> Self::Reg;
    #[doc(hidden)]
    fn token_stop(self, reg: Self::Reg) -> u32;
    #[doc(hidden)]
    fn upper(self, reg: Self::Reg) -> u32;
    #[doc(hidden)]
    fn underscore(self, reg: Self::Reg) -> u32;
    #[doc(hidden)]
    fn control(self, reg: Self::Reg) -> u32;
    #[doc(hidden)]
    fn target_stop(self, reg: Self::Reg) -> u32;

    /// The 32 bytes of `buf` at `at`; lanes past the end are marked.
    #[inline(always)]
    fn window(self, buf: &[u8], at: usize) -> Window<Self::Reg> {
        match buf.get(at..).and_then(<[u8]>::first_chunk::<WIDTH>) {
            Some(bytes) => Window {
                reg: self.load(bytes),
                shift: 0,
                past_end: 0,
            },
            None => self.tail(buf, at),
        }
    }

    /// The window of an offset less than 32 bytes from the end: the last 32 bytes of the slice moved down to the offset,
    /// or a zero padded copy when the slice is shorter than a window.
    #[inline(always)]
    fn tail(self, buf: &[u8], at: usize) -> Window<Self::Reg> {
        let len = buf.len();
        let Some(live) = len.checked_sub(at).filter(|&live| live > 0) else {
            return self.padded(&[]);
        };
        match buf.last_chunk::<WIDTH>() {
            Some(last) => Window {
                reg: self.load(last),
                shift: (WIDTH - live) as u32,
                past_end: u32::MAX << live,
            },
            None => self.padded(&buf[at..]),
        }
    }

    #[inline(always)]
    fn padded(self, live: &[u8]) -> Window<Self::Reg> {
        let mut bytes = [0; WIDTH];
        bytes[..live.len()].copy_from_slice(live);
        Window {
            reg: self.load(&bytes),
            shift: 0,
            past_end: u32::MAX << live.len(),
        }
    }

    #[inline(always)]
    fn token(self, window: &Window<Self::Reg>) -> TokenMasks {
        TokenMasks {
            stop: (self.token_stop(window.reg) >> window.shift) | window.past_end,
            upper: self.upper(window.reg) >> window.shift,
            underscore: self.underscore(window.reg) >> window.shift,
        }
    }

    /// Lanes that are below 0x20 or DEL, and the lanes past the end.
    #[inline(always)]
    fn control_mask(self, window: &Window<Self::Reg>) -> u32 {
        (self.control(window.reg) >> window.shift) | window.past_end
    }

    /// Lanes that are not a request-target byte ([`is_target_byte`]), and the lanes past the end.
    #[inline(always)]
    fn target_mask(self, window: &Window<Self::Reg>) -> u32 {
        (self.target_stop(window.reg) >> window.shift) | window.past_end
    }
}

/// A scan that runs at the level of the CPU.
pub trait Job {
    type Out;

    /// Runs the scan. Mark it `#[inline(always)]`, with everything it calls that takes `lanes`, so that it is compiled for
    /// the level; otherwise it runs, correctly, without the instruction set.
    fn run<L: Lanes>(self, lanes: L) -> Self::Out;
}

/// Runs `job` at the level of this process ([`level`]).
#[inline]
pub fn dispatch<J: Job>(job: J) -> J::Out {
    simd::dispatch(level(), job)
}

/// Runs `job` at `level`, clamped to the hardware.
pub fn dispatch_with<J: Job>(level: Level, job: J) -> J::Out {
    simd::dispatch(level.clamp(), job)
}

/// The portable lanes: byte at a time, the oracle of the vector levels.
#[derive(Clone, Copy)]
pub struct Scalar(());

impl Scalar {
    pub(crate) const fn new() -> Self {
        Self(())
    }
}

impl sealed::Sealed for Scalar {}

fn mask(bytes: &[u8; WIDTH], hit: impl Fn(u8) -> bool) -> u32 {
    bytes
        .iter()
        .enumerate()
        .fold(0, |mask, (lane, &b)| mask | u32::from(hit(b)) << lane)
}

impl Lanes for Scalar {
    type Reg = [u8; WIDTH];

    fn load(self, bytes: &[u8; WIDTH]) -> Self::Reg {
        *bytes
    }

    fn token_stop(self, reg: Self::Reg) -> u32 {
        mask(&reg, |b| !is_tchar(b))
    }

    fn upper(self, reg: Self::Reg) -> u32 {
        mask(&reg, |b| b.is_ascii_uppercase())
    }

    fn underscore(self, reg: Self::Reg) -> u32 {
        mask(&reg, |b| b == b'_')
    }

    fn control(self, reg: Self::Reg) -> u32 {
        mask(&reg, |b| b < 0x20 || b == 0x7f)
    }

    fn target_stop(self, reg: Self::Reg) -> u32 {
        mask(&reg, |b| !is_target_byte(b))
    }
}

pub(crate) fn run_scalar<J: Job>(job: J) -> J::Out {
    job.run(Scalar::new())
}
