use std::collections::VecDeque;

use super::App;
use crate::cli::{
    top::{model::Unit, views::View},
    units,
};

const CAPACITY: usize = 60;
const TRACKED: usize = 3;

#[derive(Clone, Debug, Default)]
pub(crate) struct Follow {
    target: Option<(View, String)>,
    pub(crate) series: Vec<(&'static str, VecDeque<f64>)>,
}

impl Follow {
    pub(crate) fn bars(&self, title: &str) -> Vec<u64> {
        self.series
            .iter()
            .find(|(candidate, _)| *candidate == title)
            .map(|(_, points)| {
                points
                    .iter()
                    .map(|value| units::saturating_u64(value * 100.0))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl App {
    pub(crate) fn record_follow(&mut self) {
        let Some(row) = self
            .arranged
            .as_ref()
            .and_then(|arranged| arranged.row(self.selection.position))
            .filter(|_| !self.selection.gone)
        else {
            self.follow = Follow::default();
            return;
        };
        let target = (self.view, row.key.clone());
        if self.follow.target.as_ref() != Some(&target) {
            self.follow = Follow {
                target: Some(target),
                series: Vec::new(),
            };
        }
        let columns = self.view.columns();
        let tracked = columns
            .iter()
            .zip(&row.cells)
            .filter(|(_, cell)| matches!(cell.unit(), Some(Unit::Rate | Unit::BytesRate)))
            .take(TRACKED);
        for (column, cell) in tracked {
            let value = cell.number_value().unwrap_or(0.0);
            let position = self
                .follow
                .series
                .iter()
                .position(|(title, _)| *title == column.title)
                .unwrap_or_else(|| {
                    self.follow.series.push((column.title, VecDeque::new()));
                    self.follow.series.len() - 1
                });
            let points = &mut self.follow.series[position].1;
            if points.len() == CAPACITY {
                points.pop_front();
            }
            points.push_back(value);
        }
    }
}
