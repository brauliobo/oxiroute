use std::collections::VecDeque;

use crate::{
    cli::{top::totals::Totals, units},
    observability::stats_page::model::StatsSnapshot,
};

const CAPACITY: usize = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Point {
    pub(crate) requests: f64,
    pub(crate) errors: f64,
    pub(crate) bytes_in: f64,
    pub(crate) bytes_out: f64,
    pub(crate) connections: f64,
    pub(crate) cpu: f64,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Series {
    points: VecDeque<Point>,
}

fn per_second(now: u64, before: u64, seconds: f64) -> f64 {
    units::as_f64(now.saturating_sub(before)) / seconds
}

impl Series {
    pub(crate) fn push(&mut self, before: &StatsSnapshot, now: &StatsSnapshot) {
        let elapsed = now
            .sampled_at_unix_ms
            .saturating_sub(before.sampled_at_unix_ms);
        if elapsed == 0 {
            return;
        }
        let seconds = units::as_f64(elapsed) / 1_000.0;
        let (then, current) = (Totals::of(before), Totals::of(now));
        if self.points.len() == CAPACITY {
            self.points.pop_front();
        }
        self.points.push_back(Point {
            requests: per_second(current.requests, then.requests, seconds),
            errors: per_second(current.errors, then.errors, seconds),
            bytes_in: per_second(current.bytes_in, then.bytes_in, seconds),
            bytes_out: per_second(current.bytes_out, then.bytes_out, seconds),
            connections: units::as_f64(now.process.active_connections),
            cpu: now.process.cpu_percent.unwrap_or_default(),
        });
    }

    pub(crate) fn clear(&mut self) {
        self.points.clear();
    }

    pub(crate) fn last(&self) -> Option<&Point> {
        self.points.back()
    }

    pub(crate) fn peak(&self, pick: fn(&Point) -> f64) -> f64 {
        self.points.iter().map(pick).fold(0.0, f64::max)
    }

    pub(crate) fn bars(&self, pick: fn(&Point) -> f64) -> Vec<u64> {
        self.points
            .iter()
            .map(|point| units::saturating_u64(pick(point) * 100.0))
            .collect()
    }
}
