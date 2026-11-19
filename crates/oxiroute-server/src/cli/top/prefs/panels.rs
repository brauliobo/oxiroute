use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PanelState {
    #[default]
    Expanded,
    Collapsed,
    Hidden,
}

impl PanelState {
    pub(crate) const fn collapse_toggled(self) -> Self {
        match self {
            Self::Expanded => Self::Collapsed,
            Self::Collapsed | Self::Hidden => Self::Expanded,
        }
    }

    pub(crate) const fn hide_toggled(self) -> Self {
        match self {
            Self::Hidden => Self::Expanded,
            Self::Expanded | Self::Collapsed => Self::Hidden,
        }
    }

    pub(crate) const fn shown(self) -> bool {
        !matches!(self, Self::Hidden)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Panel {
    Meters,
    Summary,
    Drawer,
}

impl Panel {
    pub(crate) const ALL: [Self; 3] = [Self::Meters, Self::Summary, Self::Drawer];

    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Meters => "Meters",
            Self::Summary => "Summary",
            Self::Drawer => "Detail",
        }
    }

    pub(crate) const fn key(self) -> char {
        match self {
            Self::Meters => 'h',
            Self::Summary => 's',
            Self::Drawer => 'i',
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub(crate) struct Panels {
    pub(crate) meters: PanelState,
    pub(crate) summary: PanelState,
    pub(crate) drawer: PanelState,
}

impl Default for Panels {
    fn default() -> Self {
        Self {
            meters: PanelState::Expanded,
            summary: PanelState::Expanded,
            drawer: PanelState::Collapsed,
        }
    }
}

impl Panels {
    pub(crate) const fn get(self, panel: Panel) -> PanelState {
        match panel {
            Panel::Meters => self.meters,
            Panel::Summary => self.summary,
            Panel::Drawer => self.drawer,
        }
    }

    pub(crate) fn set(&mut self, panel: Panel, state: PanelState) {
        match panel {
            Panel::Meters => self.meters = state,
            Panel::Summary => self.summary = state,
            Panel::Drawer => self.drawer = state,
        }
    }

    pub(crate) fn hidden(self) -> Vec<Panel> {
        Panel::ALL
            .into_iter()
            .filter(|panel| !self.get(*panel).shown())
            .collect()
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Density {
    #[default]
    Normal,
    Compact,
}

impl Density {
    pub(crate) const fn toggled(self) -> Self {
        match self {
            Self::Normal => Self::Compact,
            Self::Compact => Self::Normal,
        }
    }
}
