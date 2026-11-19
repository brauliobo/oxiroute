#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Column {
    pub(crate) key: &'static str,
    pub(crate) title: &'static str,
    pub(crate) numeric: bool,
    pub(crate) priority: u8,
    pub(crate) sum: bool,
}

impl Column {
    pub(crate) const fn text(key: &'static str, title: &'static str, priority: u8) -> Self {
        Self {
            key,
            title,
            numeric: false,
            priority,
            sum: false,
        }
    }

    pub(crate) const fn number(key: &'static str, title: &'static str, priority: u8) -> Self {
        Self {
            key,
            title,
            numeric: true,
            priority,
            sum: false,
        }
    }

    pub(crate) const fn summed(mut self) -> Self {
        self.sum = true;
        self
    }

    pub(crate) fn named(&self, name: &str) -> bool {
        self.key.eq_ignore_ascii_case(name) || self.title.eq_ignore_ascii_case(name)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Sort {
    pub(crate) column: usize,
    pub(crate) descending: bool,
}

impl Sort {
    pub(crate) const fn ascending(mut self) -> Self {
        self.descending = false;
        self
    }

    pub(crate) fn for_column(columns: &[Column], column: usize) -> Self {
        Self {
            column,
            descending: columns.get(column).is_some_and(|column| column.numeric),
        }
    }
}
