use std::time::Duration;

use super::{
    keys::Key,
    rates::Sampling,
    rows::{Sort, Table, View},
    source::Sample,
};
use crate::observability::stats_page::model::StatsSnapshot;

const PAGE: usize = 10;
const MIN_WINDOW_MS: u64 = 1_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Effect {
    Idle,
    Quit,
    Retarget,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Control {
    pub(super) paused: bool,
    pub(super) requests: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Mode {
    Browse,
    Filter(String),
}

pub(super) struct App {
    pub(super) view: View,
    pub(super) sorts: [Sort; View::ALL.len()],
    pub(super) filter: String,
    pub(super) mode: Mode,
    pub(super) paused: bool,
    pub(super) selected: usize,
    pub(super) interval: Duration,
    pub(super) current: Option<Sample>,
    pub(super) previous: Option<StatsSnapshot>,
    pub(super) error: Option<String>,
}

impl App {
    pub(super) fn new(view: View, sort: Sort, filter: String, interval: Duration) -> Self {
        let mut sorts = View::ALL.map(View::default_sort);
        sorts[view.index()] = sort;
        Self {
            view,
            sorts,
            filter,
            mode: Mode::Browse,
            paused: false,
            selected: 0,
            interval,
            current: None,
            previous: None,
            error: None,
        }
    }

    pub(super) fn control(&self) -> Control {
        Control {
            paused: self.paused,
            requests: self.view == View::Requests,
        }
    }

    pub(super) fn sort(&self) -> Sort {
        self.sorts[self.view.index()]
    }

    pub(super) fn sample(&mut self, sample: Result<Sample, String>) {
        if self.paused {
            return;
        }
        match sample {
            Ok(sample) => {
                let too_close = self.current.as_ref().is_some_and(|current| {
                    sample
                        .snapshot
                        .sampled_at_unix_ms
                        .saturating_sub(current.snapshot.sampled_at_unix_ms)
                        < MIN_WINDOW_MS
                });
                if !too_close {
                    self.previous = self.current.take().map(|current| current.snapshot);
                }
                self.current = Some(sample);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }

    pub(super) fn table(&self) -> Option<Table> {
        let current = self.current.as_ref()?;
        let sampling = Sampling::new(&current.snapshot, self.previous.as_ref());
        let table = self.view.table(&sampling, current.requests.as_ref());
        Some(table.arranged(self.sort(), &self.filter))
    }

    pub(super) fn clamp(&mut self, rows: usize) {
        self.selected = self.selected.min(rows.saturating_sub(1));
    }

    pub(super) fn key(&mut self, key: Key) -> Effect {
        match &mut self.mode {
            Mode::Filter(draft) => match key {
                Key::Char(character) => draft.push(character),
                Key::Backspace => {
                    draft.pop();
                }
                Key::Enter => {
                    self.filter = std::mem::take(draft);
                    self.mode = Mode::Browse;
                    self.selected = 0;
                }
                Key::Escape => {
                    self.filter.clear();
                    self.mode = Mode::Browse;
                    self.selected = 0;
                }
                Key::Interrupt => return Effect::Quit,
                _ => {}
            },
            Mode::Browse => return self.browse(key),
        }
        Effect::Idle
    }

    fn browse(&mut self, key: Key) -> Effect {
        match key {
            Key::Char('q') | Key::Escape | Key::Interrupt => return Effect::Quit,
            Key::Tab => return self.show(self.view.next()),
            Key::BackTab => return self.show(self.view.previous()),
            Key::Char('1') => return self.show(View::Listeners),
            Key::Char('2') => return self.show(View::Pools),
            Key::Char('3') => return self.show(View::Servers),
            Key::Char('4') => return self.show(View::Requests),
            Key::Char('p' | ' ') => {
                self.paused = !self.paused;
                return Effect::Retarget;
            }
            Key::Char('/') => self.mode = Mode::Filter(self.filter.clone()),
            Key::Char('>' | '.') => self.move_sort(1),
            Key::Char('<' | ',') => self.move_sort(-1),
            Key::Char('r') => self.sorts[self.view.index()].descending ^= true,
            Key::Up | Key::Char('k') => self.selected = self.selected.saturating_sub(1),
            Key::Down | Key::Char('j') => self.selected = self.selected.saturating_add(1),
            Key::PageUp => self.selected = self.selected.saturating_sub(PAGE),
            Key::PageDown => self.selected = self.selected.saturating_add(PAGE),
            Key::Home | Key::Char('g') => self.selected = 0,
            Key::End | Key::Char('G') => self.selected = usize::MAX,
            _ => {}
        }
        Effect::Idle
    }

    fn show(&mut self, view: View) -> Effect {
        self.view = view;
        self.selected = 0;
        Effect::Retarget
    }

    fn move_sort(&mut self, step: isize) {
        let columns = self.view.columns().len();
        let column = (self.sort().column + columns).saturating_add_signed(step) % columns;
        self.sorts[self.view.index()] = Sort::for_column(self.view, column);
    }
}
