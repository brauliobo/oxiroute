mod columns;
mod panels;
mod store;
mod views;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub(crate) use columns::ColumnPrefs;
pub(crate) use panels::{Density, Panel, PanelState, Panels};
pub(crate) use store::Store;
pub(crate) use views::ViewPrefs;

use super::{history::Window, layout::Tier, model::Units, theme::ThemeName, views::View};

pub(crate) const VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Prefs {
    pub(crate) version: u32,
    pub(crate) panels: Panels,
    pub(crate) views: ViewPrefs,
    pub(crate) columns: BTreeMap<View, ColumnPrefs>,
    pub(crate) density: Density,
    pub(crate) theme: ThemeName,
    pub(crate) window: Window,
    pub(crate) units: Units,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            version: VERSION,
            panels: Panels::default(),
            views: ViewPrefs::default(),
            columns: BTreeMap::new(),
            density: Density::default(),
            theme: ThemeName::default(),
            window: Window::default(),
            units: Units::default(),
        }
    }
}

impl Prefs {
    pub(crate) fn columns_of(&self, view: View) -> ColumnPrefs {
        self.columns.get(&view).cloned().unwrap_or_default()
    }

    pub(crate) fn visible(&self, view: View, tier: Tier) -> Vec<usize> {
        self.columns_of(view).effective(view, tier)
    }

    pub(crate) fn edit_columns(&mut self, view: View, edit: impl FnOnce(&mut ColumnPrefs)) {
        let prefs = self.columns.entry(view).or_default();
        edit(prefs);
        if *prefs == ColumnPrefs::default() {
            self.columns.remove(&view);
        }
    }
}
