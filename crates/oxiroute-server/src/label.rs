//! Names shared by many records: route ids and pool names.
//!
//! A request copies a label by bumping a count, and the access log writes it without searching it for credential
//! markers again: that was decided once, when the plan that owns the label was built.

use std::{ops::Deref, sync::Arc};

use crate::observability::logging::redact_text_cow;

#[derive(Clone, Debug)]
pub(crate) struct Label {
    text: Arc<str>,
    /// Only when redaction changes the text, which is rare.
    redacted: Option<Arc<str>>,
}

impl Label {
    pub(crate) fn new(text: &str) -> Self {
        let redacted = redact_text_cow(text);
        Self {
            redacted: (redacted != text).then(|| Arc::from(&*redacted)),
            text: Arc::from(text),
        }
    }

    /// The label for a record that outlives a borrow.
    pub(crate) fn shared(&self) -> Arc<str> {
        Arc::clone(&self.text)
    }

    /// Whether `text` is this label's own shared text.
    pub(crate) fn is_shared(&self, text: &Arc<str>) -> bool {
        Arc::ptr_eq(&self.text, text)
    }

    /// The text as the access log writes it.
    pub(crate) fn logged(&self) -> &str {
        self.redacted.as_deref().unwrap_or(&self.text)
    }
}

impl Deref for Label {
    type Target = str;

    fn deref(&self) -> &str {
        &self.text
    }
}

impl From<&str> for Label {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

impl From<String> for Label {
    fn from(text: String) -> Self {
        Self::new(&text)
    }
}

/// The labels of the servers a request tried, in order. A request almost always tries one server, which is held in
/// place; only a retry allocates.
#[derive(Debug, Default)]
pub(crate) enum Labels {
    #[default]
    None,
    One(Label),
    Many(Vec<Label>),
}

impl Labels {
    pub(crate) fn push(&mut self, label: Label) {
        *self = match std::mem::take(self) {
            Self::None => Self::One(label),
            Self::One(first) => Self::Many(vec![first, label]),
            Self::Many(mut labels) => {
                labels.push(label);
                Self::Many(labels)
            }
        };
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::None;
    }
}

impl Deref for Labels {
    type Target = [Label];

    fn deref(&self) -> &[Label] {
        match self {
            Self::None => &[],
            Self::One(label) => std::slice::from_ref(label),
            Self::Many(labels) => labels,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_keep_the_order_they_were_pushed_in_whatever_their_number() {
        let mut labels = Labels::default();
        assert!(labels.is_empty() && labels.last().is_none());
        for (count, name) in ["a", "b", "c"].into_iter().enumerate() {
            labels.push(Label::new(name));
            let names = labels.iter().map(|label| &**label).collect::<Vec<_>>();
            assert_eq!(names, ["a", "b", "c"][..=count]);
            assert_eq!(labels.last().map(|label| &**label), Some(name));
        }
        labels.clear();
        assert!(labels.is_empty());
    }

    #[test]
    fn a_clean_label_is_logged_as_it_is_and_a_credential_looking_one_is_redacted() {
        let clean = Label::new("api-pool");
        assert_eq!((&*clean, clean.logged()), ("api-pool", "api-pool"));
        let odd = Label::new("secret-token-pool");
        assert_eq!(&*odd, "secret-token-pool");
        assert_eq!(odd.logged(), crate::observability::logging::REDACTED);
        assert_eq!(&*odd.shared(), "secret-token-pool");
    }
}
