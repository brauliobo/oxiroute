mod clock;
mod sampling;
mod series;
mod window;

use std::collections::VecDeque;

pub(crate) use clock::Clock;
pub(crate) use sampling::Sampling;
pub(crate) use series::{Point, Series};
pub(crate) use window::Window;

use super::{source::Sample, views::View};

const RETAINED_MS: u64 = 65_000;
const RETAINED_SAMPLES: usize = 72;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Pushed {
    Added,
    Reset,
    Stale,
}

pub(crate) struct Baseline<'a> {
    pub(crate) sample: &'a Sample,
    pub(crate) complete: bool,
}

#[derive(Default)]
pub(crate) struct History {
    samples: VecDeque<Sample>,
    series: Series,
}

fn at(sample: &Sample) -> u64 {
    sample.snapshot.sampled_at_unix_ms
}

impl History {
    pub(crate) fn push(&mut self, sample: Sample) -> Pushed {
        let outcome = match self.samples.back() {
            Some(last) if at(&sample) <= at(last) => return Pushed::Stale,
            Some(last) if restarted(last, &sample) => Pushed::Reset,
            _ => Pushed::Added,
        };
        if outcome == Pushed::Reset {
            self.clear();
        }
        if let Some(last) = self.samples.back() {
            self.series.push(&last.snapshot, &sample.snapshot);
        }
        let now = at(&sample);
        self.samples.push_back(sample);
        while self.samples.len() > RETAINED_SAMPLES
            || self
                .samples
                .front()
                .is_some_and(|oldest| now - at(oldest) > RETAINED_MS)
        {
            self.samples.pop_front();
        }
        outcome
    }

    pub(crate) fn clear(&mut self) {
        self.samples.clear();
        self.series.clear();
    }

    pub(crate) fn latest(&self) -> Option<&Sample> {
        self.samples.back()
    }

    pub(crate) const fn series(&self) -> &Series {
        &self.series
    }

    pub(crate) fn sampling(&self, window: Window, view: View) -> Option<Sampling<'_>> {
        let latest = self.latest()?;
        let baseline = self.baseline(window, |sample| view.usable(sample));
        Some(Sampling::new(latest, baseline, window))
    }

    pub(crate) fn baseline(
        &self,
        window: Window,
        usable: impl Fn(&Sample) -> bool,
    ) -> Option<Baseline<'_>> {
        let span = window.millis()?;
        let latest = self.samples.back()?;
        let target = at(latest).saturating_sub(span);
        let earlier = || {
            self.samples
                .iter()
                .take(self.samples.len() - 1)
                .filter(|sample| usable(sample))
        };
        match earlier().rfind(|sample| at(sample) <= target) {
            Some(sample) => Some(Baseline {
                sample,
                complete: true,
            }),
            None => earlier().next().map(|sample| Baseline {
                sample,
                complete: false,
            }),
        }
    }
}

fn restarted(last: &Sample, next: &Sample) -> bool {
    last.snapshot.process.uptime_ms > next.snapshot.process.uptime_ms
        || last.snapshot.generation.number != next.snapshot.generation.number
}
