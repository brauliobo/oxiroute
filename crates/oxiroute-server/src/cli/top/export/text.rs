use std::fmt::Write as _;

use super::{Plan, Printed};
use crate::cli::top::{
    headline,
    history::{Sampling, Window},
    model::Row,
};

fn basis(sampling: &Sampling) -> String {
    match (sampling.window, sampling.complete) {
        (Window::Life, _) => "rates over the uptime of the server".to_owned(),
        (window, true) => format!("rates over a {} window", window.label()),
        (window, false) => format!(
            "rates over less than the {} window: not enough samples yet",
            window.label()
        ),
    }
}

pub(super) fn render(sampling: &Sampling, plan: &Plan, tables: &[Printed]) -> String {
    let mut text = String::new();
    if plan.prefs.panels.meters.shown() {
        text = headline::lines(sampling, plan.units).join("\n");
        write!(text, "\n{}", basis(sampling)).expect("writing to a string");
    }
    for printed in tables {
        write!(
            text,
            "\n\n{}\n{}",
            printed.view.title(),
            plain(plan, printed)
        )
        .expect("writing to a string");
    }
    text.push('\n');
    text.trim_start_matches('\n').to_owned()
}

fn plain(plan: &Plan, printed: &Printed) -> String {
    let columns = printed.view.columns();
    let table = &printed.arranged.table;
    let rows = (0..printed.arranged.len()).filter_map(|position| printed.arranged.row(position));
    let pinned = [table.other.as_ref(), table.total.as_ref()]
        .into_iter()
        .flatten();
    let cells = |row: &Row| {
        printed
            .columns
            .iter()
            .enumerate()
            .map(|(slot, index)| {
                let text = row.cells[*index].render(plan.units);
                if slot == 0 {
                    format!("{}{text}", "  ".repeat(usize::from(row.depth)))
                } else {
                    text
                }
            })
            .collect::<Vec<_>>()
    };
    let mut lines = vec![
        printed
            .columns
            .iter()
            .map(|index| columns[*index].title.to_owned())
            .collect::<Vec<_>>(),
    ];
    lines.extend(rows.map(&cells));
    lines.extend(pinned.map(cells));
    let widths = (0..printed.columns.len())
        .map(|slot| {
            lines
                .iter()
                .map(|line| line[slot].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect::<Vec<_>>();
    let mut out = lines
        .iter()
        .map(|line| {
            let padded = line.iter().enumerate().map(|(slot, cell)| {
                let width = widths[slot];
                if columns[printed.columns[slot]].numeric {
                    format!("{cell:>width$}")
                } else {
                    format!("{cell:<width$}")
                }
            });
            padded.collect::<Vec<_>>().join(" ").trim_end().to_owned()
        })
        .collect::<Vec<_>>();
    if let Some(notice) = &table.notice {
        out.insert(1, format!("  {}", notice.text()));
    }
    out.join("\n")
}
