use crate::cli::units;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Filling,
    Delta { now_ms: u64, before_ms: u64 },
    Life { now_ms: u64, uptime_ms: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Clock {
    mode: Mode,
}

impl Clock {
    pub(crate) const fn filling() -> Self {
        Self {
            mode: Mode::Filling,
        }
    }

    pub(crate) const fn delta(now_ms: u64, before_ms: u64) -> Self {
        Self {
            mode: if before_ms < now_ms {
                Mode::Delta { now_ms, before_ms }
            } else {
                Mode::Filling
            },
        }
    }

    pub(crate) const fn life(now_ms: u64, uptime_ms: u64) -> Self {
        Self {
            mode: Mode::Life { now_ms, uptime_ms },
        }
    }

    pub(crate) const fn windowed(&self) -> bool {
        matches!(self.mode, Mode::Delta { .. })
    }

    pub(crate) fn change(&self, now: u64, before: Option<u64>) -> Option<u64> {
        match self.mode {
            Mode::Filling => None,
            Mode::Delta { .. } => now.checked_sub(before?),
            Mode::Life { .. } => Some(now),
        }
    }

    pub(crate) fn rate(&self, now: u64, before: Option<u64>, born_ms: Option<u64>) -> Option<f64> {
        match (self.mode, before) {
            (Mode::Filling, _) => None,
            (Mode::Delta { now_ms, before_ms }, Some(before)) => {
                per_second(now.checked_sub(before)?, now_ms - before_ms)
            }
            (Mode::Delta { now_ms, before_ms }, None) => {
                let born = born_ms.filter(|born| *born > before_ms)?;
                per_second(now, now_ms.saturating_sub(born))
            }
            (Mode::Life { now_ms, uptime_ms }, _) => per_second(
                now,
                born_ms.map_or(uptime_ms, |born| now_ms.saturating_sub(born)),
            ),
        }
    }
}

fn per_second(delta: u64, elapsed_ms: u64) -> Option<f64> {
    (elapsed_ms > 0).then(|| units::as_f64(delta) * 1_000.0 / units::as_f64(elapsed_ms))
}
