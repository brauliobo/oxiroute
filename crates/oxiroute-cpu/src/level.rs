//! The instruction-set level this process dispatches on: detected once, optionally lowered by `OXIROUTE_CPU_LEVEL`.

use core::sync::atomic::{AtomicU8, Ordering};

/// Environment variable that lowers the level (`scalar`, `sse42`, `avx2` or `avx512`); it never raises it.
pub const ENV: &str = "OXIROUTE_CPU_LEVEL";

/// What the CPU can run, ordered: every level includes the ones below it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Scalar,
    /// SSSE3, SSE4.1, SSE4.2 and POPCNT.
    Sse42,
    /// AVX2, BMI1 and BMI2.
    Avx2,
    /// AVX-512 F, BW and VL.
    Avx512,
}

impl Level {
    pub const ALL: [Self; 4] = [Self::Scalar, Self::Sse42, Self::Avx2, Self::Avx512];

    #[must_use]
    pub const fn name(self) -> &'static str {
        ["scalar", "sse42", "avx2", "avx512"][self as usize]
    }

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.name() == name)
    }

    /// The highest level of this CPU; always [`Level::Scalar`] off x86-64 or without the `std` feature.
    #[must_use]
    pub fn hardware() -> Self {
        match HARDWARE.load(Ordering::Relaxed) {
            UNSET => {
                let hardware = detect();
                HARDWARE.store(hardware as u8, Ordering::Relaxed);
                hardware
            }
            n => Self::ALL[usize::from(n)],
        }
    }

    /// `self`, or the hardware level when that is lower.
    #[must_use]
    pub fn clamp(self) -> Self {
        self.min(Self::hardware())
    }

    /// Every level this CPU can run, lowest first.
    pub fn supported() -> impl Iterator<Item = Self> {
        Self::ALL
            .into_iter()
            .filter(|level| *level <= Self::hardware())
    }
}

#[cfg(all(target_arch = "x86_64", feature = "std"))]
fn detect() -> Level {
    use std::arch::is_x86_feature_detected as has;
    let sse42 = has!("ssse3") && has!("sse4.1") && has!("sse4.2") && has!("popcnt");
    let avx2 = sse42 && has!("avx2") && has!("bmi1") && has!("bmi2");
    let avx512 = avx2 && has!("avx512f") && has!("avx512bw") && has!("avx512vl");
    [Level::Scalar, Level::Sse42, Level::Avx2, Level::Avx512]
        [usize::from(sse42) + usize::from(avx2) + usize::from(avx512)]
}

#[cfg(not(all(target_arch = "x86_64", feature = "std")))]
fn detect() -> Level {
    Level::Scalar
}

const UNSET: u8 = u8::MAX;
static HARDWARE: AtomicU8 = AtomicU8::new(UNSET);
static LEVEL: AtomicU8 = AtomicU8::new(UNSET);

/// The level the scanners run at: the hardware level, lowered by `OXIROUTE_CPU_LEVEL`. Resolved on first use.
///
/// # Panics
/// On first use, when `OXIROUTE_CPU_LEVEL` is set to anything but a level name.
#[inline]
pub fn level() -> Level {
    match LEVEL.load(Ordering::Relaxed) {
        UNSET => resolve(),
        n => Level::ALL[usize::from(n)],
    }
}

#[cold]
fn resolve() -> Level {
    let level = lowered(Level::hardware(), requested());
    LEVEL.store(level as u8, Ordering::Relaxed);
    level
}

fn lowered(hardware: Level, requested: Option<Level>) -> Level {
    requested.map_or(hardware, |level| level.min(hardware))
}

#[cfg(feature = "std")]
fn requested() -> Option<Level> {
    match std::env::var(ENV) {
        Ok(name) => Some(
            Level::parse(&name)
                .unwrap_or_else(|| panic!("{ENV}={name}: expected scalar, sse42, avx2 or avx512")),
        ),
        Err(std::env::VarError::NotPresent) => None,
        Err(err) => panic!("{ENV}: {err}"),
    }
}

#[cfg(not(feature = "std"))]
fn requested() -> Option<Level> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_only_lower_the_level() {
        for hardware in Level::ALL {
            assert_eq!(lowered(hardware, None), hardware);
            for requested in Level::ALL {
                assert_eq!(lowered(hardware, Some(requested)), requested.min(hardware));
            }
        }
    }

    #[test]
    fn names_round_trip() {
        for level in Level::ALL {
            assert_eq!(Level::parse(level.name()), Some(level));
        }
        assert_eq!(Level::parse("avx"), None);
    }

    #[test]
    fn clamp_never_exceeds_the_hardware() {
        assert!(
            Level::ALL
                .into_iter()
                .all(|level| level.clamp() <= Level::hardware())
        );
        assert_eq!(Level::Scalar.clamp(), Level::Scalar);
        assert_eq!(Level::supported().last(), Some(Level::hardware()));
    }
}
