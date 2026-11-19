mod clients;
mod conns;
mod generations;
mod listeners;
mod requests;
mod rtmp;
mod traffic;
mod upstreams;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use super::{
    history::Sampling,
    model::{Column, Notice, Sort, Table, Tone},
    source::{Feed, Sample},
};

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub(crate) enum View {
    Listeners,
    #[value(alias = "pools", alias = "servers")]
    Upstreams,
    Clients,
    Conns,
    Requests,
    Rtmp,
    Generations,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Chip {
    pub(crate) label: &'static str,
    pub(crate) value: String,
    pub(crate) tone: Tone,
}

impl Chip {
    pub(crate) fn new(label: &'static str, value: impl Into<String>) -> Self {
        Self {
            label,
            value: value.into(),
            tone: Tone::Plain,
        }
    }

    pub(crate) fn toned(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Detail {
    pub(crate) lines: Vec<(&'static str, String)>,
}

impl Detail {
    pub(crate) fn with(mut self, label: &'static str, value: impl Into<String>) -> Self {
        self.lines.push((label, value.into()));
        self
    }
}

fn ready<T>(feed: &Feed<T>) -> Result<&T, Notice> {
    match feed {
        Feed::Ready(value) => Ok(value),
        Feed::Unavailable(notice) => Err(notice.clone()),
        Feed::Off => Err(Notice::Loading),
    }
}

pub(crate) fn word<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

impl View {
    pub(crate) const ALL: [Self; 7] = [
        Self::Listeners,
        Self::Upstreams,
        Self::Clients,
        Self::Conns,
        Self::Requests,
        Self::Rtmp,
        Self::Generations,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Listeners => "listeners",
            Self::Upstreams => "upstreams",
            Self::Clients => "clients",
            Self::Conns => "conns",
            Self::Requests => "requests",
            Self::Rtmp => "rtmp",
            Self::Generations => "generations",
        }
    }

    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Listeners => "Listeners",
            Self::Upstreams => "Upstreams",
            Self::Clients => "Clients",
            Self::Conns => "Conns",
            Self::Requests => "Requests",
            Self::Rtmp => "RTMP",
            Self::Generations => "Generations",
        }
    }

    pub(crate) const fn columns(self) -> &'static [Column] {
        match self {
            Self::Listeners => listeners::COLUMNS,
            Self::Upstreams => upstreams::COLUMNS,
            Self::Clients => clients::COLUMNS,
            Self::Conns => conns::COLUMNS,
            Self::Requests => requests::COLUMNS,
            Self::Rtmp => rtmp::COLUMNS,
            Self::Generations => generations::COLUMNS,
        }
    }

    pub(crate) fn table(self, sampling: &Sampling) -> Table {
        match self {
            Self::Listeners => listeners::table(sampling),
            Self::Upstreams => upstreams::table(sampling),
            Self::Clients => clients::table(sampling),
            Self::Conns => conns::table(sampling),
            Self::Requests => requests::table(sampling),
            Self::Rtmp => rtmp::table(sampling),
            Self::Generations => generations::table(sampling),
        }
    }

    pub(crate) fn summary(self, sampling: &Sampling) -> Vec<Chip> {
        match self {
            Self::Listeners => listeners::summary(sampling),
            Self::Upstreams => upstreams::summary(sampling),
            Self::Clients => clients::summary(sampling),
            Self::Conns => conns::summary(sampling),
            Self::Requests => requests::summary(sampling),
            Self::Rtmp => rtmp::summary(sampling),
            Self::Generations => generations::summary(sampling),
        }
    }

    pub(crate) fn detail(self, sampling: &Sampling, key: &str) -> Detail {
        match self {
            Self::Listeners => listeners::detail(sampling, key),
            Self::Upstreams => upstreams::detail(sampling, key),
            Self::Clients => clients::detail(sampling, key),
            Self::Conns => conns::detail(sampling, key),
            Self::Requests => requests::detail(sampling, key),
            Self::Rtmp => rtmp::detail(sampling, key),
            Self::Generations => generations::detail(sampling, key),
        }
    }

    pub(crate) fn badge(self, sample: &Sample) -> Option<(String, Tone)> {
        match self {
            Self::Listeners => Some(listeners::badge(sample)),
            Self::Upstreams => Some(upstreams::badge(sample)),
            Self::Clients => clients::badge(sample),
            Self::Conns => conns::badge(sample),
            Self::Requests => requests::badge(sample),
            Self::Rtmp => rtmp::badge(sample),
            Self::Generations => Some(generations::badge(sample)),
        }
    }

    pub(crate) fn usable(self, sample: &Sample) -> bool {
        match self {
            Self::Clients => sample.clients.ready().is_some(),
            Self::Conns => sample.conns.ready().is_some(),
            Self::Rtmp => sample.rtmp.ready().is_some(),
            Self::Requests | Self::Listeners | Self::Upstreams | Self::Generations => true,
        }
    }

    pub(crate) const fn grouping(self) -> Option<&'static str> {
        match self {
            Self::Clients | Self::Conns | Self::Requests => Some("listener"),
            _ => None,
        }
    }

    pub(crate) fn default_sort(self) -> Sort {
        let key = match self {
            Self::Requests | Self::Conns => "age",
            Self::Clients => "in",
            _ => return Sort::for_column(self.columns(), 0).ascending(),
        };
        let column = self.column_named(key).unwrap_or(0);
        Sort::for_column(self.columns(), column)
    }

    pub(crate) fn column_named(self, name: &str) -> Option<usize> {
        self.columns().iter().position(|column| column.named(name))
    }
}
