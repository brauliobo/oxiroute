use super::{App, Layer, actions::Action};
use crate::cli::top::prefs::ViewPrefs;

impl App {
    pub(crate) fn picker_len(&self) -> usize {
        match self.layers.last() {
            Some(Layer::Columns(_)) => self.view.columns().len(),
            Some(Layer::Views(_)) => crate::cli::top::views::View::ALL.len(),
            Some(Layer::Help(_)) => super::help_lines().len(),
            _ => 0,
        }
    }

    pub(crate) fn pick(&mut self, action: Action) {
        let len = self.picker_len();
        let Some(Layer::Columns(cursor) | Layer::Views(cursor) | Layer::Help(cursor)) =
            self.layers.last_mut()
        else {
            return;
        };
        match action {
            Action::PickerUp => *cursor = cursor.saturating_sub(1),
            Action::PickerDown => *cursor = (*cursor + 1).min(len.saturating_sub(1)),
            Action::PickerClick(item) => *cursor = item.min(len.saturating_sub(1)),
            _ => {}
        }
        let cursor = *cursor;
        match action {
            Action::PickerToggle | Action::PickerClick(_) => self.picker_toggle(cursor),
            Action::PickerMoveUp => self.picker_shift(cursor, -1),
            Action::PickerMoveDown => self.picker_shift(cursor, 1),
            Action::PickerReset => self.picker_reset(),
            _ => {}
        }
    }

    fn picker_toggle(&mut self, cursor: usize) {
        let view = self.view;
        let tier = self.tier();
        match self.layers.last() {
            Some(Layer::Columns(_)) => {
                let index = self.prefs.columns_of(view).ordered(view)[cursor];
                self.prefs
                    .edit_columns(view, |columns| columns.toggle(view, index, tier));
            }
            Some(Layer::Views(_)) => {
                let target = self.prefs.views.ordered()[cursor];
                self.prefs.views.toggle_hidden(target);
            }
            _ => return,
        }
        self.changed_prefs();
        self.reset_widths();
    }

    fn picker_shift(&mut self, cursor: usize, step: isize) {
        let view = self.view;
        let moved = match self.layers.last() {
            Some(Layer::Columns(_)) => {
                let before = self.prefs.columns_of(view).ordered(view);
                let index = before[cursor];
                self.prefs
                    .edit_columns(view, |columns| columns.shift(view, index, step));
                self.prefs.columns_of(view).ordered(view) != before
            }
            Some(Layer::Views(_)) => {
                let before = self.prefs.views.ordered();
                let target = before[cursor];
                self.prefs.views.shift(target, step);
                self.prefs.views.ordered() != before
            }
            _ => return,
        };
        if moved {
            if let Some(Layer::Columns(at) | Layer::Views(at)) = self.layers.last_mut() {
                *at = at.saturating_add_signed(step);
            }
            self.changed_prefs();
            self.reset_widths();
        }
    }

    fn picker_reset(&mut self) {
        match self.layers.last() {
            Some(Layer::Columns(_)) => {
                self.prefs.columns.remove(&self.view);
            }
            Some(Layer::Views(_)) => self.prefs.views = ViewPrefs::default(),
            _ => return,
        }
        self.changed_prefs();
        self.reset_widths();
    }
}
