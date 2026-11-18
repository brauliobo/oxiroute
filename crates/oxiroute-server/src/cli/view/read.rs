use serde_json::Value;

pub(super) fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

pub(super) fn number(value: &Value, key: &str) -> Option<u64> {
    let field = value.get(key)?;
    field.as_u64().or_else(|| field.as_str()?.parse().ok())
}

pub(super) fn flag(value: &Value, key: &str) -> Option<bool> {
    value.get(key)?.as_bool()
}

pub(super) fn items<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

pub(super) fn joined(value: &Value, key: &str) -> Option<String> {
    let names = items(value, key)
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    (!names.is_empty()).then(|| names.join(","))
}
