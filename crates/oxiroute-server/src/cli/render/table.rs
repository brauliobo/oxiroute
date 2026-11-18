use std::fmt::Write as _;

use serde_json::Value;

use super::plain;

pub(super) fn render(value: &Value) -> String {
    let mut text = String::new();
    let Some(object) = value.as_object() else {
        plain::write_value(&mut text, value);
        return text;
    };
    let Some((_, rows)) = object
        .iter()
        .find_map(|(name, value)| value.as_array().map(|rows| (name, rows)))
    else {
        for (name, value) in object {
            let _ = writeln!(text, "{name}\t{}", cell(value));
        }
        return text;
    };
    let mut columns = Vec::<&str>::new();
    for row in rows {
        if let Some(row) = row.as_object() {
            for name in row.keys() {
                if !columns.contains(&name.as_str()) {
                    columns.push(name);
                }
            }
        }
    }
    if columns.is_empty() {
        for row in rows {
            plain::write_value(&mut text, row);
        }
        return text;
    }
    let _ = writeln!(text, "{}", columns.join("\t"));
    for row in rows {
        if let Some(row) = row.as_object() {
            let values = columns
                .iter()
                .map(|column| row.get(*column).map_or_else(String::new, cell))
                .collect::<Vec<_>>();
            let _ = writeln!(text, "{}", values.join("\t"));
        }
    }
    text
}

fn cell(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
        .replace(['\n', '\r', '\t'], " ")
}
