use std::{borrow::Cow, fmt::Write as _};

use crate::cli::units;

const ABSENT: &str = "-";
const SHORT_REVISION: usize = 12;
const COLUMN_GAP: &str = "  ";

pub(crate) struct Field {
    key: Cow<'static, str>,
    unit: Option<&'static str>,
    human: String,
    raw: Option<String>,
    numeric: bool,
}

impl Field {
    fn new(key: impl Into<Cow<'static, str>>, human: String, raw: Option<String>) -> Self {
        Self {
            key: key.into(),
            unit: None,
            human,
            raw,
            numeric: false,
        }
    }

    fn measured(
        key: &'static str,
        unit: Option<&'static str>,
        value: Option<u64>,
        human: fn(u64) -> String,
    ) -> Self {
        Self {
            key: key.into(),
            unit,
            human: value.map_or_else(|| ABSENT.to_owned(), human),
            raw: value.map(|value| value.to_string()),
            numeric: true,
        }
    }

    pub(crate) fn text(key: impl Into<Cow<'static, str>>, value: Option<&str>) -> Self {
        Self::new(
            key,
            value.map_or_else(|| ABSENT.to_owned(), str::to_owned),
            value.map(str::to_owned),
        )
    }

    pub(crate) fn revision(key: &'static str, value: Option<&str>) -> Self {
        Self::new(
            key,
            value.map_or_else(
                || ABSENT.to_owned(),
                |value| value.chars().take(SHORT_REVISION).collect(),
            ),
            value.map(str::to_owned),
        )
    }

    pub(crate) fn timestamp(key: &'static str, unix_ms: Option<u64>) -> Self {
        let mut field = Self::measured(key, Some("unix_ms"), unix_ms, units::timestamp);
        field.numeric = false;
        field
    }

    pub(crate) fn shown(key: &'static str, human: String, raw: Option<String>) -> Self {
        Self::new(key, human, raw)
    }

    pub(crate) fn flag(key: &'static str, value: Option<bool>) -> Self {
        Self::new(
            key,
            value.map_or_else(
                || ABSENT.to_owned(),
                |value| if value { "yes" } else { "no" }.to_owned(),
            ),
            value.map(|value| value.to_string()),
        )
    }

    pub(crate) fn number(key: &'static str, value: Option<u64>) -> Self {
        Self::measured(key, None, value, |value| value.to_string())
    }

    pub(crate) fn count(key: &'static str, value: Option<u64>) -> Self {
        Self::measured(key, None, value, units::count)
    }

    pub(crate) fn bytes(key: &'static str, value: Option<u64>) -> Self {
        Self::measured(key, Some("bytes"), value, units::bytes)
    }

    pub(crate) fn millis(key: &'static str, value: Option<u64>) -> Self {
        Self::measured(key, Some("ms"), value, units::millis)
    }

    pub(crate) fn duration(key: &'static str, value: Option<u64>) -> Self {
        Self::measured(key, Some("ms"), value, units::duration)
    }

    pub(crate) fn percent(key: &'static str, value: Option<f64>) -> Self {
        let mut field = Self::new(
            key,
            value.map_or_else(|| ABSENT.to_owned(), |value| format!("{value:.1}%")),
            value.map(|value| format!("{value:.1}")),
        );
        field.unit = Some("pct");
        field.numeric = true;
        field
    }

    fn label(&self) -> String {
        self.key.replace('_', " ")
    }

    fn plain_key(&self) -> String {
        self.unit.map_or_else(
            || self.key.to_string(),
            |unit| format!("{}_{unit}", self.key),
        )
    }
}

pub(crate) type Record = Vec<Field>;

pub(crate) enum Block {
    Record {
        name: Cow<'static, str>,
        fields: Record,
    },
    Rows {
        name: Cow<'static, str>,
        rows: Vec<Record>,
    },
}

pub(crate) struct Document {
    blocks: Vec<Block>,
    notices: Vec<String>,
}

pub(crate) struct Text {
    pub(crate) stdout: String,
    pub(crate) notices: Vec<String>,
}

impl Document {
    pub(crate) const fn new(blocks: Vec<Block>) -> Self {
        Self {
            blocks,
            notices: Vec::new(),
        }
    }

    pub(crate) fn record(name: &'static str, fields: Record) -> Self {
        Self::new(vec![Block::Record {
            name: name.into(),
            fields,
        }])
    }

    pub(crate) fn rows(name: &'static str, rows: Vec<Record>) -> Self {
        Self::new(vec![Block::Rows {
            name: name.into(),
            rows,
        }])
    }

    pub(crate) fn noting(mut self, notice: Option<String>) -> Self {
        self.notices.extend(notice);
        self
    }

    pub(crate) fn table(&self) -> Text {
        let mut text = Text {
            stdout: String::new(),
            notices: Vec::new(),
        };
        for block in &self.blocks {
            match block {
                Block::Record { fields, .. } => {
                    separate(&mut text.stdout);
                    write_record(&mut text.stdout, fields);
                }
                Block::Rows { name, rows } if rows.is_empty() => {
                    text.notices.push(format!("no {}", name.replace('_', " ")));
                }
                Block::Rows { rows, .. } => {
                    separate(&mut text.stdout);
                    write_rows(&mut text.stdout, rows);
                }
            }
        }
        text.notices.extend(self.notices.iter().cloned());
        text
    }

    pub(crate) fn plain(&self) -> String {
        let mut text = String::new();
        let named = self.blocks.len() > 1;
        for block in &self.blocks {
            match block {
                Block::Record { name, fields } => {
                    write_pairs(&mut text, named.then_some(name), fields);
                }
                Block::Rows { name, rows } => {
                    for row in rows {
                        write_pairs(&mut text, named.then_some(name), row);
                    }
                }
            }
        }
        text
    }
}

fn separate(text: &mut String) {
    if !text.is_empty() {
        text.push('\n');
    }
}

fn write_record(text: &mut String, fields: &[Field]) {
    let width = fields
        .iter()
        .map(|field| field.label().len())
        .max()
        .unwrap_or(0);
    for field in fields {
        let _ = writeln!(text, "{:<width$}{COLUMN_GAP}{}", field.label(), field.human);
    }
}

fn write_rows(text: &mut String, rows: &[Record]) {
    let headers = rows[0]
        .iter()
        .map(|field| field.label().to_uppercase())
        .collect::<Vec<_>>();
    if headers.is_empty() {
        return;
    }
    let widths = (0..headers.len())
        .map(|column| {
            rows.iter()
                .map(|row| row[column].human.chars().count())
                .chain([headers[column].chars().count()])
                .max()
                .unwrap_or(0)
        })
        .collect::<Vec<_>>();
    let numeric = rows[0]
        .iter()
        .map(|field| field.numeric)
        .collect::<Vec<_>>();
    let cells = |values: Vec<&str>| {
        let last = values.len() - 1;
        let line = values
            .iter()
            .enumerate()
            .map(|(column, value)| match (numeric[column], column == last) {
                (true, _) => format!("{value:>width$}", width = widths[column]),
                (false, true) => (*value).to_owned(),
                (false, false) => format!("{value:<width$}", width = widths[column]),
            })
            .collect::<Vec<_>>()
            .join(COLUMN_GAP);
        format!("{}\n", line.trim_end())
    };
    text.push_str(&cells(headers.iter().map(String::as_str).collect()));
    for row in rows {
        text.push_str(&cells(
            row.iter().map(|field| field.human.as_str()).collect(),
        ));
    }
}

fn write_pairs(text: &mut String, record: Option<&str>, fields: &[Field]) {
    if fields.is_empty() {
        return;
    }
    let mut pairs = record
        .map(|name| format!("record={name}"))
        .into_iter()
        .collect::<Vec<_>>();
    pairs.extend(fields.iter().filter_map(|field| {
        field
            .raw
            .as_deref()
            .map(|raw| format!("{}={}", field.plain_key(), quoted(raw)))
    }));
    let _ = writeln!(text, "{}", pairs.join(" "));
}

fn quoted(value: &str) -> String {
    let bare = !value.is_empty()
        && !value
            .chars()
            .any(|character| character.is_whitespace() || matches!(character, '"' | '=' | '\\'));
    if bare {
        value.to_owned()
    } else {
        serde_json::Value::from(value).to_string()
    }
}

#[cfg(test)]
mod tests;
