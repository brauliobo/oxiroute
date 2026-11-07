#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn print_value(value: &Value, output: Output, quiet: bool) -> Result<(), CliError> {
    if quiet {
        return Ok(());
    }
    match output {
        Output::Json => println!(
            "{}",
            serde_json::to_string(value).map_err(|error| CliError::Local(error.to_string()))?
        ),
        Output::Table => print_table(value),
        Output::Plain => print_plain(value),
    }
    Ok(())
}

fn print_table(value: &Value) {
    let Some(object) = value.as_object() else {
        print_plain(value);
        return;
    };
    let Some((_, rows)) = object
        .iter()
        .find_map(|(name, value)| value.as_array().map(|rows| (name, rows)))
    else {
        for (name, value) in object {
            println!("{name}\t{}", table_cell(value));
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
        rows.iter().for_each(print_plain);
        return;
    }
    println!("{}", columns.join("\t"));
    for row in rows {
        if let Some(row) = row.as_object() {
            let values = columns
                .iter()
                .map(|column| row.get(*column).map_or_else(String::new, table_cell))
                .collect::<Vec<_>>();
            println!("{}", values.join("\t"));
        }
    }
}

fn table_cell(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
        .replace(['\n', '\r', '\t'], " ")
}

fn print_plain(value: &Value) {
    match value {
        Value::Null => println!("null"),
        Value::Bool(value) => println!("{value}"),
        Value::Number(value) => println!("{value}"),
        Value::String(value) => println!("{value}"),
        Value::Array(values) => values.iter().for_each(print_plain),
        Value::Object(values) => {
            for (name, value) in values {
                if value.is_object() || value.is_array() {
                    println!("{name}={value}");
                } else if let Some(value) = value.as_str() {
                    println!("{name}={value}");
                } else {
                    println!("{name}={value}");
                }
            }
        }
    }
}
