use super::cell::{Cell, Tone, Value};
use super::column::Column;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Row {
    pub(crate) key: String,
    pub(crate) cells: Vec<Cell>,
    pub(crate) parent: Option<usize>,
    pub(crate) depth: u8,
    pub(crate) search: String,
}

impl Row {
    pub(crate) fn new(key: impl Into<String>, cells: Vec<Cell>, names: &[&str]) -> Self {
        Self {
            key: key.into(),
            cells,
            parent: None,
            depth: 0,
            search: names.join(" ").to_ascii_lowercase(),
        }
    }

    pub(crate) const fn under(mut self, parent: usize, depth: u8) -> Self {
        self.parent = Some(parent);
        self.depth = depth;
        self
    }

    pub(crate) fn summary(label: &str, cells: Vec<Cell>) -> Self {
        Self::new(label, cells, &[label])
    }

    pub(crate) fn folded(key: &str, label: &str, rows: &[&Self], columns: &[Column]) -> Self {
        let cells = columns
            .iter()
            .enumerate()
            .map(|(index, column)| match (index, column.sum) {
                (0, _) => Cell::text(label),
                (_, true) => sum(rows.iter().map(|row| &row.cells[index])),
                _ => Cell::text(""),
            })
            .collect();
        Self::new(key, cells, &[label])
    }
}

fn sum<'a>(cells: impl Iterator<Item = &'a Cell>) -> Cell {
    let mut unit = None;
    let mut total = 0.0;
    for cell in cells {
        if let Value::Number {
            value, unit: found, ..
        } = cell.value
        {
            unit = Some(found);
            total += value;
        }
    }
    unit.map_or_else(Cell::missing, |unit| {
        Cell::number(total, unit).toned(Tone::Plain)
    })
}
