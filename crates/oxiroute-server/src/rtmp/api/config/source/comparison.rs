use serde_json::{Value, json};

use crate::{config::diff::changes, rtmp::api::dto::redact_callback_url};

const REDACTED: &str = "<redacted>";

/// How two configurations differ, with inline secrets hidden: the values of a secret setting
/// are replaced in both directions, so a changed secret still shows as changed.
pub(super) fn comparison(revision: Option<&str>, old: &Value, new: &Value) -> Value {
    let (mut found, truncated) = changes(old, new);
    for change in &mut found {
        for value in [&mut change.old, &mut change.new].into_iter().flatten() {
            redact(&change.path, value);
        }
    }
    json!({
        "revision": revision,
        "equal": found.is_empty(),
        "changes": found,
        "truncated": truncated,
    })
}

fn redact(path: &str, value: &mut Value) {
    match value {
        Value::String(text) if is_token_secret(path) => REDACTED.clone_into(text),
        Value::String(text) if is_callback(path) => *text = redact_callback_url(text),
        Value::Object(entries) => {
            for (key, entry) in entries {
                redact(&format!("{path}/{key}"), entry);
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                redact(&format!("{path}/{index}"), item);
            }
        }
        _ => {}
    }
}

fn is_token_secret(path: &str) -> bool {
    path.ends_with("/token/secret")
}

fn is_callback(path: &str) -> bool {
    path.rsplit_once('/').is_some_and(|(parent, last)| {
        last.starts_with("on_") && parent.rsplit('/').next() == Some("callbacks")
    })
}
