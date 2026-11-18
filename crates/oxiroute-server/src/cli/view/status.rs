use serde_json::Value;

use super::{
    generation::with_restart_banner,
    listeners,
    read::{flag, items, number, text},
};
use crate::cli::render::{Block, Document, Field};

pub(super) fn show(value: &Value) -> Document {
    let component = |name| {
        value
            .get("components")
            .and_then(|components| components.get(name))
            .and_then(|component| text(component, "state"))
    };
    let degraded = flag(value, "degraded");
    let overall = degraded.map(|degraded| if degraded { "degraded" } else { "healthy" });
    let supervision = value
        .pointer("/capabilities/supervision/mode")
        .and_then(Value::as_str);
    let mut fields = vec![
        Field::text("build", text(value, "buildVersion")),
        Field::text("state", overall),
        Field::revision("active", text(value, "activeRevision")),
        Field::revision("previous", text(value, "previousRevision")),
        Field::revision("candidate", text(value, "candidateRevision")),
        Field::revision("disk", text(value, "diskRevision")),
        Field::duration("generation_age", number(value, "activeGenerationAgeMs")),
        Field::text("supervision", supervision),
        Field::text("process", component("process")),
        Field::text("host", component("host")),
        Field::text("generation", component("generation")),
        Field::text("audit", component("audit")),
    ];
    with_restart_banner(&mut fields, value);
    Document::new(vec![
        Block::Record {
            name: "status".into(),
            fields,
        },
        Block::Rows {
            name: "listeners".into(),
            rows: items(value, "listeners")
                .iter()
                .map(listeners::row)
                .collect(),
        },
    ])
}
