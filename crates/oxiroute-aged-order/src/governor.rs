use std::time::{Duration, Instant};

use crate::Age;

const SMOOTHING: f64 = 16.0;

#[derive(Default)]
pub(crate) struct Governor {
    poll_ns: f64,
    ready_avg: f64,
    last_batch: Option<(Instant, u64)>,
    idle: bool,
}

impl Governor {
    pub(crate) fn rest(&mut self) {
        self.idle = true;
    }

    #[allow(clippy::cast_precision_loss)]
    pub(crate) fn age(
        &mut self,
        multiple: f64,
        (now, polls): (Instant, u64),
        ready: usize,
    ) -> Duration {
        self.smooth_poll_time(now, polls);
        self.idle = false;
        self.last_batch = Some((now, polls));
        self.ready_avg = smoothed(self.ready_avg, ready as f64);
        let ramp =
            ((self.ready_avg - Age::RAMP_FROM) / (Age::RAMP_TO - Age::RAMP_FROM)).clamp(0.0, 1.0);
        let passes = 1.0 + (multiple - 1.0).max(0.0) * ramp;
        Duration::from_secs_f64(passes * self.ready_avg * self.poll_ns / 1e9)
            .clamp(Age::MIN, Age::MAX)
    }

    #[allow(clippy::cast_precision_loss)]
    fn smooth_poll_time(&mut self, now: Instant, polls: u64) {
        let Some((started, before)) = self.last_batch.filter(|_| !self.idle) else {
            return;
        };
        let done = polls - before;
        if done > 0 {
            let sample = now.saturating_duration_since(started).as_nanos() as f64 / done as f64;
            self.poll_ns = smoothed(self.poll_ns, sample);
        }
    }
}

fn smoothed(average: f64, sample: f64) -> f64 {
    if average == 0.0 {
        sample
    } else {
        average + (sample - average) / SMOOTHING
    }
}
