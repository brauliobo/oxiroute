use std::cmp::Ordering;

use serde::{Deserialize, Serialize};
use serde_json::{Number, Value as Json};

use crate::cli::units;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Unit {
    Count,
    Rate,
    Bytes,
    BytesRate,
    Percent,
    Millis,
    Duration,
    Plain,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Units {
    #[default]
    Bytes,
    Bits,
}

impl Units {
    pub(crate) const fn toggled(self) -> Self {
        match self {
            Self::Bytes => Self::Bits,
            Self::Bits => Self::Bytes,
        }
    }

    pub(crate) fn rate_text(self, value: f64) -> String {
        match self {
            Self::Bytes => format!("{}B/s", units::bytes_rate(value)),
            Self::Bits => format!("{}b/s", units::rate(value * 8.0)),
        }
    }

    pub(crate) fn traffic(self, value: f64, per_second: bool) -> String {
        match (self, per_second) {
            (Self::Bytes, true) => units::bytes_rate(value),
            (Self::Bytes, false) => units::bytes(units::saturating_u64(value)),
            (Self::Bits, true) => format!("{}b", units::rate(value * 8.0)),
            (Self::Bits, false) => format!("{}b", units::count(units::saturating_u64(value) * 8)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Tone {
    Plain,
    Good,
    Warn,
    Bad,
    Dim,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Missing,
    Text(String),
    Number {
        value: f64,
        unit: Unit,
        limit: Option<u64>,
        over: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Cell {
    pub(crate) value: Value,
    pub(crate) tone: Tone,
}

impl Cell {
    const fn of(value: Value) -> Self {
        Self {
            value,
            tone: Tone::Plain,
        }
    }

    pub(crate) fn text(text: impl Into<String>) -> Self {
        Self::of(Value::Text(text.into()))
    }

    pub(crate) const fn number(value: f64, unit: Unit) -> Self {
        Self::of(Value::Number {
            value,
            unit,
            limit: None,
            over: false,
        })
    }

    pub(crate) fn count(value: u64) -> Self {
        Self::number(units::as_f64(value), Unit::Count)
    }

    pub(crate) fn plain(value: u64) -> Self {
        Self::number(units::as_f64(value), Unit::Plain)
    }

    pub(crate) fn bytes(value: u64) -> Self {
        Self::number(units::as_f64(value), Unit::Bytes)
    }

    pub(crate) fn millis(value: u64) -> Self {
        Self::number(units::as_f64(value), Unit::Millis)
    }

    pub(crate) fn duration(value_ms: u64) -> Self {
        Self::number(units::as_f64(value_ms), Unit::Duration)
    }

    pub(crate) fn over(value_ms: u64) -> Self {
        Self::of(Value::Number {
            value: units::as_f64(value_ms) + 1.0,
            unit: Unit::Millis,
            limit: None,
            over: true,
        })
    }

    pub(crate) fn limited(value: u64, limit: Option<u64>) -> Self {
        Self::of(Value::Number {
            value: units::as_f64(value),
            unit: Unit::Plain,
            limit,
            over: false,
        })
    }

    pub(crate) const fn missing() -> Self {
        Self {
            value: Value::Missing,
            tone: Tone::Dim,
        }
    }

    fn measured(value: Option<f64>, unit: Unit) -> Self {
        value.map_or_else(Self::missing, |value| Self::number(value, unit))
    }

    pub(crate) fn rate(value: Option<f64>) -> Self {
        Self::measured(value, Unit::Rate)
    }

    pub(crate) fn bytes_rate(value: Option<f64>) -> Self {
        Self::measured(value, Unit::BytesRate)
    }

    pub(crate) fn percent(value: Option<f64>) -> Self {
        Self::measured(value, Unit::Percent)
    }

    pub(crate) fn optional_text(value: Option<&str>) -> Self {
        value.map_or_else(Self::missing, Self::text)
    }

    pub(crate) const fn toned(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    pub(crate) fn number_value(&self) -> Option<f64> {
        match self.value {
            Value::Number { value, .. } => Some(value),
            _ => None,
        }
    }

    pub(crate) const fn unit(&self) -> Option<Unit> {
        match self.value {
            Value::Number { unit, .. } => Some(unit),
            _ => None,
        }
    }

    pub(crate) fn render(&self, units_mode: Units) -> String {
        match &self.value {
            Value::Missing => "-".into(),
            Value::Text(text) => text.clone(),
            Value::Number {
                value,
                unit,
                limit,
                over,
            } => {
                let text = number(*value, *unit, units_mode);
                match (limit, over) {
                    (Some(limit), _) => format!("{text}/{}", units::count(*limit)),
                    (None, true) => {
                        format!(">{}", units::millis(units::saturating_u64(*value - 1.0)))
                    }
                    (None, false) => text,
                }
            }
        }
    }

    pub(crate) fn ordering(&self, other: &Self) -> Ordering {
        match (&self.value, &other.value) {
            (Value::Number { value: left, .. }, Value::Number { value: right, .. }) => {
                left.total_cmp(right)
            }
            (Value::Text(left), Value::Text(right)) => left
                .bytes()
                .map(|byte| byte.to_ascii_lowercase())
                .cmp(right.bytes().map(|byte| byte.to_ascii_lowercase())),
            _ => Ordering::Equal,
        }
    }

    pub(crate) const fn is_missing(&self) -> bool {
        matches!(self.value, Value::Missing)
    }

    pub(crate) fn json(&self) -> Json {
        match &self.value {
            Value::Missing => Json::Null,
            Value::Text(text) => Json::String(text.clone()),
            Value::Number { value, .. } => {
                Number::from_f64(*value).map_or(Json::Null, Json::Number)
            }
        }
    }
}

fn number(value: f64, unit: Unit, units_mode: Units) -> String {
    match unit {
        Unit::Plain => format!("{value:.0}"),
        Unit::Count => units::count(units::saturating_u64(value)),
        Unit::Rate => units::rate(value),
        Unit::Bytes => units_mode.traffic(value, false),
        Unit::BytesRate => units_mode.traffic(value, true),
        Unit::Percent if value > 0.0 && value < 10.0 => format!("{value:.1}%"),
        Unit::Percent => format!("{value:.0}%"),
        Unit::Millis => units::millis(units::saturating_u64(value)),
        Unit::Duration => units::duration(units::saturating_u64(value)),
    }
}
