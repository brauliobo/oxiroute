use super::keymap::Scope;
use crate::cli::top::views::View;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Layer {
    Filter(String),
    Drawer,
    Help(usize),
    Columns(usize),
    Views(usize),
}

impl Layer {
    pub(crate) const fn scope(&self) -> Scope {
        match self {
            Self::Filter(_) => Scope::Filter,
            Self::Drawer => Scope::Drawer,
            Self::Help(_) => Scope::Help,
            Self::Columns(_) | Self::Views(_) => Scope::Picker,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Crumb {
    pub(crate) view: View,
    pub(crate) filter: String,
    pub(crate) selected: Option<String>,
}
