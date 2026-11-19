use std::collections::HashSet;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Groups {
    default_open: bool,
    flipped: HashSet<String>,
}

impl Groups {
    pub(crate) fn new(default_open: bool) -> Self {
        Self {
            default_open,
            flipped: HashSet::new(),
        }
    }

    pub(crate) fn is_open(&self, key: &str) -> bool {
        self.default_open != self.flipped.contains(key)
    }

    pub(crate) fn set(&mut self, key: &str, open: bool) {
        if self.is_open(key) != open {
            self.toggle(key);
        }
    }

    pub(crate) fn toggle(&mut self, key: &str) {
        if !self.flipped.remove(key) {
            self.flipped.insert(key.to_owned());
        }
    }

    pub(crate) fn open_all(&mut self) {
        self.default_open = true;
        self.flipped.clear();
    }

    pub(crate) fn close_all(&mut self) {
        self.default_open = false;
        self.flipped.clear();
    }
}
