use ratatui::layout::Rect;

use super::Scene;
use crate::cli::top::{app::Action, prefs::Panel, views::View};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Hit {
    Tab(View),
    Header(usize),
    Row(usize),
    Marker(usize),
    Chip(Action),
    Title(Panel),
    Item(usize),
    Outside,
    Nothing,
}

fn inside(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

impl Scene {
    pub(crate) fn hit(&self, x: u16, y: u16) -> Hit {
        if let Some(chip) = self.chips.iter().find(|chip| inside(chip.rect, x, y)) {
            return Hit::Chip(chip.action);
        }
        if let Some(overlay) = &self.overlay {
            if !inside(overlay.rect, x, y) {
                return Hit::Outside;
            }
            let item = overlay.items.iter().find(|item| inside(item.rect, x, y));
            return match item {
                Some(item) if overlay.clickable => Hit::Item(item.index),
                _ => Hit::Nothing,
            };
        }
        if let Some((_, panel)) = self.titles.iter().find(|(rect, _)| inside(*rect, x, y)) {
            return Hit::Title(*panel);
        }
        if let Some(tab) = self.tabs.slots.iter().find(|tab| inside(tab.rect, x, y)) {
            return Hit::Tab(tab.view);
        }
        self.hit_table(x, y)
    }

    fn hit_table(&self, x: u16, y: u16) -> Hit {
        let table = &self.table;
        if y == table.inner.y {
            let column = table
                .columns
                .iter()
                .find(|column| x >= column.x && x < column.x + column.width);
            return column.map_or(Hit::Nothing, |column| Hit::Header(column.index));
        }
        let Some(row) = table.rows.iter().find(|row| row.y == y) else {
            return Hit::Nothing;
        };
        let position = row.position.unwrap_or_default();
        match row.marker {
            Some(marker) if inside(marker, x, y) => Hit::Marker(position),
            _ if x >= table.inner.x && x < table.inner.x + table.inner.width => Hit::Row(position),
            _ => Hit::Nothing,
        }
    }
}
