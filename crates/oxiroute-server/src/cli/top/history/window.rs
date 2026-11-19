use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
pub(crate) enum Window {
    #[value(name = "2s")]
    #[serde(rename = "2s")]
    Short,
    #[default]
    #[value(name = "10s")]
    #[serde(rename = "10s")]
    Medium,
    #[value(name = "60s")]
    #[serde(rename = "60s")]
    Long,
    #[value(name = "life")]
    #[serde(rename = "life")]
    Life,
}

impl Window {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Short => "2s",
            Self::Medium => "10s",
            Self::Long => "60s",
            Self::Life => "life",
        }
    }

    pub(crate) const fn millis(self) -> Option<u64> {
        match self {
            Self::Short => Some(2_000),
            Self::Medium => Some(10_000),
            Self::Long => Some(60_000),
            Self::Life => None,
        }
    }

    pub(crate) const fn next(self) -> Self {
        match self {
            Self::Short => Self::Medium,
            Self::Medium => Self::Long,
            Self::Long => Self::Life,
            Self::Life => Self::Short,
        }
    }
}
