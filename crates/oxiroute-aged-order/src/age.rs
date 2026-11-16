use std::time::Duration;

/// How long a ready item may wait behind newer ones before it is served first.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Age {
    Fixed(Duration),
    /// Up to this many times the time one pass over the ready items takes (their number times the recent time per
    /// poll), between [`Age::MIN`] and [`Age::MAX`]. With few ready items the age is one pass, so the order stays close
    /// to first-in first-out; it grows to the full multiple between [`Age::RAMP_FROM`] and [`Age::RAMP_TO`] ready items.
    Adaptive(f64),
}

impl Age {
    pub const DEFAULT_PASSES: f64 = 3.0;

    #[must_use]
    pub fn adaptive() -> Self {
        Self::Adaptive(Self::DEFAULT_PASSES)
    }

    pub const MIN: Duration = Duration::from_micros(200);
    pub const RAMP_FROM: f64 = 64.0;
    pub const RAMP_TO: f64 = 256.0;
    pub const MAX: Duration = Duration::from_secs(1);

    pub(crate) fn initial(self) -> Duration {
        match self {
            Self::Fixed(age) => age,
            Self::Adaptive(_) => Self::MIN,
        }
    }
}
