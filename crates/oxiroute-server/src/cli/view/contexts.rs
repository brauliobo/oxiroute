use serde_json::Value;

use super::read::{flag, items, text};
use crate::cli::render::{Document, Field};

pub(super) fn list(value: &Value) -> Document {
    let rows = items(value, "contexts")
        .iter()
        .map(|entry| {
            let source = text(entry, "tokenSource")
                .zip(text(entry, "tokenRef"))
                .map(|(kind, reference)| format!("{kind}:{reference}"));
            vec![
                Field::text(
                    "current",
                    Some(if flag(entry, "current") == Some(true) {
                        "*"
                    } else {
                        ""
                    }),
                ),
                Field::text("name", text(entry, "name")),
                Field::text("endpoint", text(entry, "endpoint")),
                Field::text("token", source.as_deref()),
                Field::text("timeout", text(entry, "timeout")),
            ]
        })
        .collect();
    Document::rows("contexts", rows)
}
