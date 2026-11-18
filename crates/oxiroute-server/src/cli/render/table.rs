use serde_json::Value;

use super::plain;

pub(super) fn print(value: &Value) {
    let Some(object) = value.as_object() else {
        plain::print(value);
        return;
    };
    let Some((_, rows)) = object
        .iter()
        .find_map(|(name, value)| value.as_array().map(|rows| (name, rows)))
    else {
        for (name, value) in object {
            println!("{name}\t{}", cell(value));
        }
        return;
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
        rows.iter().for_each(plain::print);
        return;
    }
    println!("{}", columns.join("\t"));
    for row in rows {
        if let Some(row) = row.as_object() {
            let values = columns
                .iter()
                .map(|column| row.get(*column).map_or_else(String::new, cell))
                .collect::<Vec<_>>();
            println!("{}", values.join("\t"));
        }
    }
}

fn cell(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
        .replace(['\n', '\r', '\t'], " ")
}
