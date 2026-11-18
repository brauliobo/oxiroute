use serde_json::Value;

use super::read::{flag, items, number, text};
use crate::cli::render::{Block, Document, Field, Record};

const SECTIONS: [&str; 9] = [
    "listeners",
    "upstream_pools",
    "http_services",
    "forward_proxy_services",
    "rtmp_services",
    "l4_services",
    "certificates",
    "tls_profiles",
    "cache_stores",
];

pub(super) fn show(value: &Value) -> Document {
    let config = value.get("config").unwrap_or(&Value::Null);
    let mut fields = vec![
        Field::text("format", text(value, "configFormat")),
        Field::revision("disk", text(value, "diskRevision")),
        Field::revision("active", text(value, "activeRevision")),
        Field::revision("candidate", text(value, "candidateRevision")),
        Field::flag("compositional", flag(value, "compositional")),
        Field::count("dependencies", number(value, "dependencyCount")),
    ];
    fields
        .extend(SECTIONS.map(|section| {
            Field::count(section, u64::try_from(items(config, section).len()).ok())
        }));
    let mut blocks = vec![Block::Record {
        name: "config".into(),
        fields,
    }];
    let diagnostics = items(value, "diagnostics");
    if !diagnostics.is_empty() {
        blocks.push(Block::Rows {
            name: "diagnostics".into(),
            rows: diagnostics.iter().map(diagnostic).collect(),
        });
    }
    Document::new(blocks)
}

fn diagnostic(diagnostic: &Value) -> Record {
    vec![
        Field::text("code", text(diagnostic, "code")),
        Field::text("stage", text(diagnostic, "stage")),
        Field::text("message", text(diagnostic, "message")),
    ]
}
