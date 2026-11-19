use super::App;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Selection {
    pub(crate) key: Option<String>,
    pub(crate) position: usize,
    pub(crate) gone: bool,
}

impl App {
    pub(crate) fn selected_key(&self) -> Option<&str> {
        self.selection.key.as_deref()
    }

    pub(crate) fn selected_is_parent(&self) -> bool {
        self.arranged
            .as_ref()
            .is_some_and(|arranged| arranged.is_parent(self.selection.position))
    }

    pub(crate) fn reselect(&mut self) {
        let Some(arranged) = &self.arranged else {
            return;
        };
        let rows = arranged.len();
        if rows == 0 {
            self.selection = Selection::default();
            return;
        }
        match self
            .selection
            .key
            .as_deref()
            .and_then(|key| arranged.position(key))
        {
            Some(position) => {
                self.selection.position = position;
                self.selection.gone = false;
            }
            None if self.selection.key.is_none() => self.select_at(0),
            None => {
                self.selection.position = self.selection.position.min(rows - 1);
                self.selection.gone = true;
            }
        }
    }

    pub(crate) fn select_at(&mut self, position: usize) {
        let Some(arranged) = &self.arranged else {
            return;
        };
        let position = position.min(arranged.len().saturating_sub(1));
        self.selection = Selection {
            key: arranged.row(position).map(|row| row.key.clone()),
            position,
            gone: false,
        };
    }

    pub(crate) fn move_selection(&mut self, step: isize) {
        let target = self.selection.position.saturating_add_signed(step);
        self.select_at(target);
    }

    pub(crate) fn viewport(&self) -> usize {
        self.scene.table.viewport
    }
}
