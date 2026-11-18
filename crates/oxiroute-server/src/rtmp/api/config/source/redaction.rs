use oxiroute_config::ConfigDraft;

use crate::rtmp::api::dto::{redact_callback_url, rtmp_token_secrets, secret_callback_urls};

const MIN_REDACTABLE_BYTES: usize = 8;
const PLACEHOLDER_PREFIX: &str = "<redacted:";
const CALLBACK_PLACEHOLDER: &str = "<redacted>";

/// The inline secrets of a configuration, found in authored text by exact match.
///
/// An RTMP token secret is replaced by `<redacted:POINTER>`, where the pointer names the setting
/// that holds it, and a callback URL by the redacted form the typed API uses. Restoring reverses
/// both against the stored configuration, so a text that comes back with its placeholders keeps
/// the secrets it had.
pub(super) struct SourceSecrets {
    tokens: Vec<(String, String)>,
    urls: Vec<(String, String)>,
}

pub(super) struct Redacted {
    pub(super) text: String,
    pub(super) replaced: usize,
}

pub(super) struct UnresolvedPlaceholder;

impl SourceSecrets {
    pub(super) fn of(configs: &[&ConfigDraft]) -> Self {
        let mut tokens: Vec<(String, String)> = Vec::new();
        let mut urls: Vec<(String, String)> = Vec::new();
        for config in configs {
            for (pointer, secret) in rtmp_token_secrets(config) {
                let placeholder = format!("{PLACEHOLDER_PREFIX}{pointer}>");
                if !tokens
                    .iter()
                    .any(|entry| *entry == (placeholder.clone(), secret.clone()))
                {
                    tokens.push((placeholder, secret));
                }
            }
            for url in secret_callback_urls(config) {
                if !urls.iter().any(|(known, _)| *known == url) {
                    let redacted = redact_callback_url(&url);
                    urls.push((url, redacted));
                }
            }
        }
        Self { tokens, urls }
    }

    pub(super) fn can_redact(&self) -> bool {
        self.tokens
            .iter()
            .map(|(_, secret)| secret)
            .chain(self.urls.iter().map(|(url, _)| url))
            .all(|secret| secret.len() >= MIN_REDACTABLE_BYTES && !secret.contains(['"', '\\']))
    }

    pub(super) fn redact(&self, text: &str) -> Redacted {
        let mut text = text.to_owned();
        let mut replaced = 0;
        for (url, redacted) in longest_first(&self.urls, |(url, _)| url) {
            replaced += text.matches(url.as_str()).count();
            text = text.replace(url.as_str(), redacted);
        }
        for (placeholder, secret) in longest_first(&self.tokens, |(_, secret)| secret) {
            replaced += text.matches(secret.as_str()).count();
            text = text.replace(secret.as_str(), placeholder);
        }
        Redacted { text, replaced }
    }

    pub(super) fn restore(&self, text: &str) -> Result<String, UnresolvedPlaceholder> {
        let mut text = text.to_owned();
        for (url, redacted) in &self.urls {
            text = text.replace(redacted.as_str(), url);
        }
        for (placeholder, secret) in &self.tokens {
            text = text.replace(placeholder.as_str(), secret);
        }
        if text.contains(PLACEHOLDER_PREFIX) || text.contains(CALLBACK_PLACEHOLDER) {
            return Err(UnresolvedPlaceholder);
        }
        Ok(text)
    }
}

fn longest_first<T>(entries: &[T], key: impl Fn(&T) -> &String) -> Vec<&T> {
    let mut ordered = entries.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|entry| std::cmp::Reverse(key(entry).len()));
    ordered
}
