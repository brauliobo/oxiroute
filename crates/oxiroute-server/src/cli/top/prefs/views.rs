use serde::{Deserialize, Serialize};

use crate::cli::top::views::View;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub(crate) struct ViewPrefs {
    pub(crate) order: Vec<View>,
    pub(crate) hidden: Vec<View>,
}

impl ViewPrefs {
    pub(crate) fn ordered(&self) -> Vec<View> {
        let mut order: Vec<View> = Vec::new();
        for view in self.order.iter().copied().chain(View::ALL) {
            if !order.contains(&view) {
                order.push(view);
            }
        }
        order
    }

    pub(crate) fn visible(&self) -> Vec<View> {
        let visible: Vec<View> = self
            .ordered()
            .into_iter()
            .filter(|view| !self.hidden.contains(view))
            .collect();
        if visible.is_empty() {
            vec![View::Listeners]
        } else {
            visible
        }
    }

    pub(crate) fn is_hidden(&self, view: View) -> bool {
        self.hidden.contains(&view)
    }

    pub(crate) fn toggle_hidden(&mut self, view: View) {
        if let Some(at) = self.hidden.iter().position(|hidden| *hidden == view) {
            self.hidden.remove(at);
        } else if self.visible().len() > 1 {
            self.hidden.push(view);
        }
    }

    pub(crate) fn shift(&mut self, view: View, step: isize) {
        let mut order = self.ordered();
        let Some(at) = order.iter().position(|candidate| *candidate == view) else {
            return;
        };
        let to = at.saturating_add_signed(step);
        if to < order.len() {
            order.swap(at, to);
            self.order = if order == View::ALL {
                Vec::new()
            } else {
                order
            };
        }
    }
}
