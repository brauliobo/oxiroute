use super::{Plan, Printed};
use crate::cli::top::model::Row;

fn field(text: &str) -> String {
    if text.contains([',', '"', '\n']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_owned()
    }
}

fn line(parts: impl IntoIterator<Item = String>) -> String {
    parts.into_iter().collect::<Vec<_>>().join(",")
}

pub(super) fn render(plan: &Plan, tables: &[Printed]) -> String {
    let Some(printed) = tables.first() else {
        return String::new();
    };
    let columns = printed.view.columns();
    let table = &printed.arranged.table;
    let mut lines = vec![line(
        ["kind".to_owned(), "key".to_owned()].into_iter().chain(
            printed
                .columns
                .iter()
                .map(|index| field(columns[*index].key)),
        ),
    )];
    let record = |kind: &str, row: &Row| {
        line(
            [kind.to_owned(), field(&row.key)].into_iter().chain(
                printed
                    .columns
                    .iter()
                    .map(|index| field(&csv_value(row, *index, plan))),
            ),
        )
    };
    lines.extend(
        (0..printed.arranged.len())
            .filter_map(|position| printed.arranged.row(position))
            .map(|row| record("row", row)),
    );
    lines.extend(table.other.iter().map(|row| record("other", row)));
    lines.extend(table.total.iter().map(|row| record("total", row)));
    format!("{}\n", lines.join("\n"))
}

fn csv_value(row: &Row, index: usize, _plan: &Plan) -> String {
    match row.cells[index].json() {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(text) => text,
        other => other.to_string(),
    }
}
