use std::cmp::Ordering;

use clap::ValueEnum;

use super::rates::Sampling;
use crate::cli::units;

mod listeners;
mod pools;
mod requests;
mod servers;
mod traffic;

pub(super) use requests::Requests;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, ValueEnum)]
pub enum View {
    Listeners,
    Pools,
    Servers,
    Requests,
}

impl View {
    pub(super) const ALL: [Self; 4] = [Self::Listeners, Self::Pools, Self::Servers, Self::Requests];

    pub(super) const fn title(self) -> &'static str {
        match self {
            Self::Listeners => "Listeners",
            Self::Pools => "Pools",
            Self::Servers => "Servers",
            Self::Requests => "Requests",
        }
    }

    pub(super) fn columns(self) -> Vec<Column> {
        match self {
            Self::Listeners => listeners::columns(),
            Self::Pools => pools::columns(),
            Self::Servers => servers::columns(),
            Self::Requests => requests::columns(),
        }
    }

    pub(super) fn table(self, sampling: &Sampling, requests: Option<&Requests>) -> Table {
        let rows = match self {
            Self::Listeners => listeners::rows(sampling),
            Self::Pools => pools::rows(sampling),
            Self::Servers => servers::rows(sampling),
            Self::Requests => requests.map(requests::rows).unwrap_or_default(),
        };
        Table { view: self, rows }
    }

    pub(super) fn default_sort(self) -> Sort {
        match self {
            Self::Requests => Sort {
                column: self.column_named("age").unwrap_or(0),
                descending: true,
            },
            _ => Sort {
                column: 0,
                descending: false,
            },
        }
    }

    pub(super) fn column_named(self, name: &str) -> Option<usize> {
        self.columns()
            .iter()
            .position(|column| column.title.eq_ignore_ascii_case(name))
    }

    pub(super) fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    pub(super) fn previous(self) -> Self {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    pub(super) fn index(self) -> usize {
        Self::ALL.iter().position(|view| *view == self).unwrap_or(0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Column {
    pub(super) title: &'static str,
    pub(super) numeric: bool,
}

impl Column {
    pub(super) const fn text(title: &'static str) -> Self {
        Self {
            title,
            numeric: false,
        }
    }

    pub(super) const fn number(title: &'static str) -> Self {
        Self {
            title,
            numeric: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Sort {
    pub(super) column: usize,
    pub(super) descending: bool,
}

impl Sort {
    pub(super) fn for_column(view: View, column: usize) -> Self {
        Self {
            column,
            descending: view
                .columns()
                .get(column)
                .is_some_and(|column| column.numeric),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Tone {
    Plain,
    Good,
    Warn,
    Bad,
    Dim,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Key {
    Missing,
    Number(f64),
    Text(String),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Cell {
    pub(super) text: String,
    pub(super) key: Key,
    pub(super) tone: Tone,
}

impl Cell {
    pub(super) fn text(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            key: Key::Text(text.to_ascii_lowercase()),
            text,
            tone: Tone::Plain,
        }
    }

    pub(super) fn number(text: impl Into<String>, value: f64) -> Self {
        Self {
            text: text.into(),
            key: Key::Number(value),
            tone: Tone::Plain,
        }
    }

    pub(super) fn count(value: u64) -> Self {
        Self::number(units::count(value), units::as_f64(value))
    }

    pub(super) fn missing() -> Self {
        Self {
            text: "-".into(),
            key: Key::Missing,
            tone: Tone::Dim,
        }
    }

    pub(super) fn measured(value: Option<f64>, format: fn(f64) -> String) -> Self {
        value.map_or_else(Self::missing, |value| Self::number(format(value), value))
    }

    pub(super) fn rate(value: Option<f64>) -> Self {
        Self::measured(value, units::rate)
    }

    pub(super) fn bytes_rate(value: Option<f64>) -> Self {
        Self::measured(value, units::bytes_rate)
    }

    pub(super) fn toned(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    pub(super) fn ordering(&self, other: &Self) -> Ordering {
        match (&self.key, &other.key) {
            (Key::Number(left), Key::Number(right)) => left.total_cmp(right),
            (Key::Text(left), Key::Text(right)) => left.cmp(right),
            _ => Ordering::Equal,
        }
    }

    const fn is_missing(&self) -> bool {
        matches!(self.key, Key::Missing)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Row {
    pub(super) cells: Vec<Cell>,
    pub(super) search: String,
}

impl Row {
    pub(super) fn new(cells: Vec<Cell>, names: &[&str]) -> Self {
        Self {
            cells,
            search: names.join(" ").to_ascii_lowercase(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Table {
    pub(super) view: View,
    pub(super) rows: Vec<Row>,
}

impl Table {
    pub(super) fn arranged(mut self, sort: Sort, filter: &str) -> Self {
        let filter = filter.to_ascii_lowercase();
        self.rows.retain(|row| row.search.contains(&filter));
        self.rows.sort_by(|left, right| {
            let (left, right) = (&left.cells[sort.column], &right.cells[sort.column]);
            match (left.is_missing(), right.is_missing()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) if sort.descending => right.ordering(left),
                (false, false) => left.ordering(right),
            }
        });
        self
    }

    pub(super) fn widths(&self) -> Vec<usize> {
        self.view
            .columns()
            .iter()
            .enumerate()
            .map(|(index, column)| {
                self.rows
                    .iter()
                    .map(|row| row.cells[index].text.chars().count())
                    .max()
                    .unwrap_or(0)
                    .max(column.title.chars().count())
            })
            .collect()
    }
}
