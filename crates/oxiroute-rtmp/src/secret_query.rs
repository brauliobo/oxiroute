use sha2::{Digest as _, Sha256};

const CONVENTIONAL_SECRET_PARAMETERS: [&str; 11] = [
    "token",
    "access_token",
    "key",
    "secret",
    "password",
    "passwd",
    "auth",
    "signature",
    "sig",
    "apikey",
    "api_key",
];

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RtmpSecretRole {
    Play,
    Publish,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RtmpSecretFingerprint {
    pub service: String,
    pub application: String,
    pub role: RtmpSecretRole,
    pub fingerprint: String,
}

pub(crate) fn secret_fingerprint(secret: impl AsRef<[u8]>) -> String {
    let digest = Sha256::digest(secret.as_ref());
    format!(
        "fp-{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3]
    )
}

pub(crate) fn redact_query(query: &str, policy_parameters: &[&str]) -> String {
    query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((name, value)) if is_secret_parameter(name, policy_parameters) => {
                format!("{name}={}", secret_fingerprint(value))
            }
            _ => pair.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn is_secret_parameter(name: &str, policy_parameters: &[&str]) -> bool {
    let name = percent_decoded(name);
    CONVENTIONAL_SECRET_PARAMETERS
        .into_iter()
        .chain(policy_parameters.iter().copied())
        .any(|secret| secret.eq_ignore_ascii_case(&name))
}

fn percent_decoded(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let escaped = (bytes[index] == b'%')
            .then(|| bytes.get(index + 1..index + 3))
            .flatten()
            .and_then(|digits| std::str::from_utf8(digits).ok())
            .and_then(|digits| u8::from_str_radix(digits, 16).ok());
        decoded.push(escaped.unwrap_or(bytes[index]));
        index += if escaped.is_some() { 3 } else { 1 };
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[cfg(test)]
mod tests;
