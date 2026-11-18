use serde_json::Value;

pub(super) fn print(value: &Value) {
    match value {
        Value::Null => println!("null"),
        Value::Bool(value) => println!("{value}"),
        Value::Number(value) => println!("{value}"),
        Value::String(value) => println!("{value}"),
        Value::Array(values) => values.iter().for_each(print),
        Value::Object(values) => {
            for (name, value) in values {
                match value.as_str() {
                    Some(value) => println!("{name}={value}"),
                    None => println!("{name}={value}"),
                }
            }
        }
    }
}
