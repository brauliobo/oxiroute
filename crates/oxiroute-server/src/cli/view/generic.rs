use serde_json::{Map, Value};

use crate::cli::render::{Block, Document, Field, Record};

type Pairs = Vec<(String, String)>;

pub(super) fn show(value: &Value) -> Document {
    match value {
        Value::Object(object) => object_document(object),
        Value::Array(entries) => Document::rows("entries", rows(entries)),
        scalar => Document::record("value", fields(&[("value".to_owned(), text_of(scalar))])),
    }
}

fn object_document(object: &Map<String, Value>) -> Document {
    let mut summary = Pairs::new();
    let mut tables = Vec::new();
    for (name, value) in object {
        match value {
            Value::Array(entries) if entries.is_empty() || entries.iter().any(Value::is_object) => {
                tables.push(Block::Rows {
                    name: snake_case(name).into(),
                    rows: rows(entries),
                });
            }
            _ => flatten(&snake_case(name), value, &mut summary),
        }
    }
    let mut blocks = Vec::new();
    if !summary.is_empty() {
        blocks.push(Block::Record {
            name: "summary".into(),
            fields: fields(&summary),
        });
    }
    blocks.extend(tables);
    Document::new(blocks)
}

fn rows(entries: &[Value]) -> Vec<Record> {
    let pairs = entries
        .iter()
        .map(|entry| {
            let mut pairs = Pairs::new();
            flatten("", entry, &mut pairs);
            pairs
        })
        .collect::<Vec<_>>();
    let mut columns = Vec::<&str>::new();
    for (key, _) in pairs.iter().flatten() {
        if !columns.contains(&key.as_str()) {
            columns.push(key);
        }
    }
    pairs
        .iter()
        .map(|row| {
            columns
                .iter()
                .map(|column| {
                    let value = row
                        .iter()
                        .find(|(key, _)| key == column)
                        .map(|(_, value)| value.as_str());
                    Field::text((*column).to_owned(), value)
                })
                .collect()
        })
        .collect()
}

fn fields(pairs: &[(String, String)]) -> Record {
    pairs
        .iter()
        .map(|(key, value)| Field::text(key.clone(), Some(value)))
        .collect()
}

fn flatten(prefix: &str, value: &Value, pairs: &mut Pairs) {
    match value {
        Value::Null => {}
        Value::Object(object) => {
            for (name, value) in object {
                let name = snake_case(name);
                let path = if prefix.is_empty() {
                    name
                } else {
                    format!("{prefix}.{name}")
                };
                flatten(&path, value, pairs);
            }
        }
        Value::Array(entries) => match entries.iter().map(scalar).collect::<Option<Vec<_>>>() {
            Some(scalars) if scalars.is_empty() => {}
            Some(scalars) => pairs.push((key_of(prefix), scalars.join(","))),
            None => {
                for (index, entry) in entries.iter().enumerate() {
                    flatten(&format!("{prefix}.{index}"), entry, pairs);
                }
            }
        },
        value => pairs.push((key_of(prefix), text_of(value))),
    }
}

fn snake_case(name: &str) -> String {
    let mut snake = String::with_capacity(name.len() + 4);
    for (index, character) in name.chars().enumerate() {
        if character.is_ascii_uppercase() {
            if index > 0 {
                snake.push('_');
            }
            snake.push(character.to_ascii_lowercase());
        } else {
            snake.push(character);
        }
    }
    snake
}

fn key_of(prefix: &str) -> String {
    if prefix.is_empty() { "value" } else { prefix }.to_owned()
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn text_of(value: &Value) -> String {
    scalar(value).unwrap_or_default()
}
