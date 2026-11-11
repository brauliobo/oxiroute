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

#[cfg(test)]
mod tests {
    use super::*;

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
