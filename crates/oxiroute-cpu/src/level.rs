//! The instruction-set level this process dispatches on: detected once, optionally lowered by `OXIROUTE_CPU_LEVEL`.

use core::sync::atomic::{AtomicU8, Ordering};

/// Environment variable that caps the level (`scalar`, `sse2`, `sse42`, `avx2` or `avx512`).
pub const ENV: &str = "OXIROUTE_CPU_LEVEL";

/// Ordered scanner backends. Hardware support is checked separately for each backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Scalar,
    /// SSE2 byte scanners; the token scanner remains scalar at this level.
    Sse2,
    /// Legacy name for the SSSE3 token scanner and SSE2 byte scanners.
    Sse42,
    /// AVX2.
    Avx2,
    /// AVX-512 F and BW.
    Avx512,
}

impl Level {
    pub const ALL: [Self; 5] = [
        Self::Scalar,
        Self::Sse2,
        Self::Sse42,
        Self::Avx2,
        Self::Avx512,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        ["scalar", "sse2", "sse42", "avx2", "avx512"][self as usize]
    }

    /// Required features, including compiler-implied prerequisites; the OS must support vector state too.
    #[must_use]
    pub const fn features(self) -> &'static [&'static str] {
        match self {
            Self::Scalar => &[],
            Self::Sse2 => &["sse2"],
            Self::Sse42 => &["sse2", "sse3", "ssse3"],
            Self::Avx2 => &["sse2", "sse3", "ssse3", "sse4.1", "sse4.2", "avx", "avx2"],
            Self::Avx512 => &[
                "sse2", "sse3", "ssse3", "sse4.1", "sse4.2", "avx", "avx2", "f16c", "fma",
                "avx512f", "avx512bw",
            ],
        }
    }

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.name() == name)
    }

    /// The highest level of this CPU; always [`Level::Scalar`] off x86-64 or without the `std` feature.
    #[must_use]
    pub fn hardware() -> Self {
        select(hardware_mask(), Self::Avx512)
    }

    /// The highest supported backend at or below `self`, including non-nested VM feature sets.
    #[must_use]
    pub fn clamp(self) -> Self {
        select(hardware_mask(), self)
    }

    /// Every level this CPU can run, lowest first.
    pub fn supported() -> impl Iterator<Item = Self> {
        let mask = hardware_mask();
        Self::ALL
            .into_iter()
            .filter(move |&level| mask & bit(level) != 0)
    }
}

const fn bit(level: Level) -> u8 {
    1 << level as u8
}

fn select(mask: u8, cap: Level) -> Level {
    // Every detector includes the scalar backend.
    Level::ALL
        .into_iter()
        .rev()
        .find(|&level| level <= cap && mask & bit(level) != 0)
        .unwrap()
}

fn hardware_mask() -> u8 {
    match HARDWARE.load(Ordering::Relaxed) {
        UNSET => {
            let mask = detect();
            HARDWARE.store(mask, Ordering::Relaxed);
            mask
        }
        mask => mask,
    }
}

#[cfg(any(test, all(target_arch = "x86_64", feature = "std")))]
fn mask_of(has: impl Fn(&str) -> bool) -> u8 {
    Level::ALL
        .into_iter()
        .filter(|level| level.features().iter().all(|&feature| has(feature)))
        .fold(0, |mask, level| mask | bit(level))
}

#[cfg(all(target_arch = "x86_64", feature = "std"))]
fn detect() -> u8 {
    mask_of(detected)
}

#[cfg(all(target_arch = "x86_64", feature = "std"))]
macro_rules! detectors {
    ($($feature:tt),*) => {
        fn detected(feature: &str) -> bool {
            match feature {
                $($feature => std::arch::is_x86_feature_detected!($feature),)*
                _ => panic!("{feature} has no runtime detector"),
            }
        }
    };
}

#[cfg(all(target_arch = "x86_64", feature = "std"))]
detectors!(
    "sse2", "sse3", "ssse3", "sse4.1", "sse4.2", "avx", "avx2", "f16c", "fma", "avx512f",
    "avx512bw"
);

#[cfg(not(all(target_arch = "x86_64", feature = "std")))]
fn detect() -> u8 {
    bit(Level::Scalar)
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
    let level = lowered(Level::hardware(), requested()).clamp();
    LEVEL.store(level as u8, Ordering::Relaxed);
    level
}

fn lowered(hardware: Level, requested: Option<Level>) -> Level {
    requested.map_or(hardware, |level| level.min(hardware))
}

#[cfg(feature = "std")]
fn requested() -> Option<Level> {
    match std::env::var(ENV) {
        Ok(name) => Some(Level::parse(&name).unwrap_or_else(|| {
            panic!("{ENV}={name}: expected scalar, sse2, sse42, avx2 or avx512")
        })),
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

    fn level_of(has: impl Fn(&str) -> bool) -> Level {
        select(mask_of(has), Level::Avx512)
    }

    fn avx2_and(extra: &'static [&'static str]) -> impl Fn(&str) -> bool {
        move |feature| Level::Avx2.features().contains(&feature) || extra.contains(&feature)
    }

    #[test]
    fn each_level_needs_every_feature_it_names() {
        for level in Level::ALL {
            assert_eq!(level_of(|f| level.features().contains(&f)), level);
            for missing in level.features() {
                let lacking = level_of(|f| f != *missing && level.features().contains(&f));
                assert!(lacking < level, "{level:?} without {missing}");
            }
        }
    }

    #[test]
    fn avx512_needs_both_foundation_and_byte_word_support() {
        assert_eq!(level_of(avx2_and(&["f16c", "fma", "avx512f"])), Level::Avx2);
        assert_eq!(
            level_of(avx2_and(&["f16c", "fma", "avx512bw"])),
            Level::Avx2
        );
        assert_eq!(level_of(avx2_and(&["avx512f", "avx512bw"])), Level::Avx2);
        assert_eq!(
            level_of(avx2_and(&["f16c", "fma", "avx512f", "avx512bw"])),
            Level::Avx512
        );
    }

    #[test]
    fn avx512_never_outranks_missing_lower_features() {
        let wide = ["avx512f", "avx512bw", "f16c", "fma"];
        assert_eq!(level_of(|f| f == "sse2" || wide.contains(&f)), Level::Sse2);
        let ssse3 = ["sse2", "sse3", "ssse3"];
        assert_eq!(
            level_of(|f| ssse3.contains(&f) || wide.contains(&f)),
            Level::Sse42
        );
    }

    #[test]
    fn a_ceiling_never_selects_beyond_the_features_present() {
        let features: [&[&str]; 3] = [
            &["avx512f", "avx512bw", "f16c", "fma"],
            &[
                "sse2",
                "avx512f",
                "avx512bw",
                "avx512vl",
                "avx512vbmi",
                "gfni",
            ],
            Level::Avx2.features(),
        ];
        let expected = [Level::Scalar, Level::Sse2, Level::Avx2];
        for (present, &capability) in features.iter().zip(&expected) {
            let mask = mask_of(|f| present.contains(&f));
            for ceiling in Level::ALL {
                assert_eq!(select(mask, ceiling), ceiling.min(capability));
            }
        }
    }

    #[test]
    fn every_declared_feature_has_a_detector() {
        #[cfg(all(target_arch = "x86_64", feature = "std"))]
        Level::ALL
            .into_iter()
            .flat_map(Level::features)
            .for_each(|&feature| {
                detected(feature);
            });
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

    #[test]
    fn every_supported_mask_honours_caps_without_assuming_intermediate_features() {
        for mask in 1..32u8 {
            let mask = mask | bit(Level::Scalar);
            for cap in Level::ALL {
                let selected = select(mask, cap);
                assert!(selected <= cap);
                assert_ne!(mask & bit(selected), 0);
                assert!(
                    Level::ALL.into_iter().all(|level| {
                        level <= selected || level > cap || mask & bit(level) == 0
                    })
                );
            }
        }
        assert_eq!(
            select(bit(Level::Scalar) | bit(Level::Avx2), Level::Sse42),
            Level::Scalar
        );
        assert_eq!(
            select(bit(Level::Scalar) | bit(Level::Avx2), Level::Avx2),
            Level::Avx2
        );
    }
}
