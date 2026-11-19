use serde::{Deserialize, Serialize};

use crate::cli::top::{layout::Tier, views::View};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub(crate) struct ColumnPrefs {
    pub(crate) order: Vec<String>,
    pub(crate) on: Vec<String>,
    pub(crate) off: Vec<String>,
}

fn default_on(priority: u8, tier: Tier) -> bool {
    match priority {
        0 | 1 => true,
        2 => tier >= Tier::Standard,
        3 => tier >= Tier::Wide,
        _ => false,
    }
}

impl ColumnPrefs {
    pub(crate) fn ordered(&self, view: View) -> Vec<usize> {
        let columns = view.columns();
        let mut indexes: Vec<usize> = self
            .order
            .iter()
            .filter_map(|key| columns.iter().position(|column| column.key == key))
            .collect();
        indexes.dedup();
        for index in 0..columns.len() {
            if !indexes.contains(&index) {
                indexes.push(index);
            }
        }
        indexes.retain(|index| *index != 0);
        indexes.insert(0, 0);
        indexes
    }

    pub(crate) fn is_on(&self, view: View, index: usize, tier: Tier) -> bool {
        let column = &view.columns()[index];
        match (
            index,
            self.on.iter().any(|key| key == column.key),
            self.off.iter().any(|key| key == column.key),
        ) {
            (0, ..) | (_, true, _) => true,
            (_, _, true) => false,
            _ => default_on(column.priority, tier),
        }
    }

    pub(crate) fn forced(&self, view: View, index: usize) -> bool {
        let key = view.columns()[index].key;
        self.on.iter().any(|candidate| candidate == key)
    }

    pub(crate) fn effective(&self, view: View, tier: Tier) -> Vec<usize> {
        self.ordered(view)
            .into_iter()
            .filter(|index| self.is_on(view, *index, tier))
            .collect()
    }

    pub(crate) fn toggle(&mut self, view: View, index: usize, tier: Tier) {
        if index == 0 {
            return;
        }
        let key = view.columns()[index].key.to_owned();
        let turn_on = !self.is_on(view, index, tier);
        self.on.retain(|candidate| *candidate != key);
        self.off.retain(|candidate| *candidate != key);
        if turn_on {
            self.on.push(key);
        } else {
            self.off.push(key);
        }
        self.normalize(view);
    }

    pub(crate) fn shift(&mut self, view: View, index: usize, step: isize) {
        let mut order = self.ordered(view);
        let Some(at) = order.iter().position(|candidate| *candidate == index) else {
            return;
        };
        let to = at.saturating_add_signed(step);
        if at == 0 || to == 0 || to >= order.len() {
            return;
        }
        order.swap(at, to);
        let columns = view.columns();
        self.order = order
            .into_iter()
            .map(|index| columns[index].key.to_owned())
            .collect();
        self.normalize(view);
    }

    fn normalize(&mut self, view: View) {
        let columns = view.columns();
        let natural = (0..columns.len())
            .map(|index| columns[index].key.to_owned())
            .collect::<Vec<_>>();
        if self.order == natural {
            self.order.clear();
        }
    }
}
