use std::collections::HashMap;

#[derive(Default)]
pub(crate) struct Widths {
    seen: HashMap<usize, u16>,
}

impl Widths {
    pub(crate) fn settle(&mut self, column: usize, wanted: u16) -> u16 {
        let width = self.seen.entry(column).or_insert(0);
        *width = (*width).max(wanted);
        *width
    }
}
