use std::fmt::Write as _;

use super::{
    rates::Sampling,
    rows::{Sort, Table, View},
    source::Sample,
    summary,
};

pub(super) fn render(sample: &Sample, views: &[View], sorts: &[Sort], filter: &str) -> String {
    let sampling = Sampling::new(&sample.snapshot, None);
    let mut text = summary::lines(&sampling).join("\n");
    for (view, sort) in views.iter().zip(sorts) {
        let table = view
            .table(&sampling, sample.requests.as_ref())
            .arranged(*sort, filter);
        write!(text, "\n\n{}\n{}", view.title(), plain(&table)).expect("writing to a string");
    }
    text.push('\n');
    text
}

fn plain(table: &Table) -> String {
    let columns = table.view.columns();
    let widths = table.widths();
    let line = |cells: Vec<&str>| {
        let padded = cells
            .iter()
            .zip(&columns)
            .zip(&widths)
            .map(|((text, column), width)| {
                if column.numeric {
                    format!("{text:>width$}")
                } else {
                    format!("{text:<width$}")
                }
            });
        padded.collect::<Vec<_>>().join(" ").trim_end().to_owned()
    };
    let header = line(columns.iter().map(|column| column.title).collect());
    let rows = table
        .rows
        .iter()
        .map(|row| line(row.cells.iter().map(|cell| cell.text.as_str()).collect()));
    std::iter::once(header)
        .chain(rows)
        .collect::<Vec<_>>()
        .join("\n")
}
