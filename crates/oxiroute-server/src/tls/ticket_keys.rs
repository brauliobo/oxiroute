use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex, PoisonError, Weak},
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use openssl::{
    rand::rand_bytes,
    symm::{Cipher, decrypt_aead, encrypt_aead},
};
use rustls::server::ProducesTickets;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

const NAME_BYTES: usize = 16;
const SECRET_BYTES: usize = 32;
const NONCE_BYTES: usize = 12;
const TAG_BYTES: usize = 16;
const LIFETIME_SECONDS: u32 = 6 * 60 * 60;

static RING: LazyLock<Mutex<HashMap<String, Weak<TicketKeys>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) struct TicketKeys {
    rotation: Mutex<Rotation>,
}

struct Rotation {
    current: Key,
    previous: Option<Key>,
    switch_at: u64,
}

struct Key {
    name: [u8; NAME_BYTES],
    secret: Zeroizing<[u8; SECRET_BYTES]>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TicketKeysState {
    profile: String,
    current: KeyState,
    previous: Option<KeyState>,
    switch_at: u64,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
struct KeyState {
    name: String,
    secret: String,
}

impl std::fmt::Debug for KeyState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("KeyState(..)")
    }
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0_u8; N];
    rand_bytes(&mut bytes).expect("OpenSSL random source");
    bytes
}

impl Key {
    fn generate() -> Self {
        Self {
            name: random(),
            secret: Zeroizing::new(random()),
        }
    }

    fn export(&self) -> KeyState {
        KeyState {
            name: STANDARD.encode(self.name),
            secret: STANDARD.encode(self.secret.as_slice()),
        }
    }

    fn import(state: &KeyState) -> Option<Self> {
        Some(Self {
            name: STANDARD.decode(&state.name).ok()?.try_into().ok()?,
            secret: Zeroizing::new(STANDARD.decode(&state.secret).ok()?.try_into().ok()?),
        })
    }

    fn seal(&self, plain: &[u8]) -> Option<Vec<u8>> {
        let nonce = random::<NONCE_BYTES>();
        let mut tag = [0_u8; TAG_BYTES];
        let encrypted = encrypt_aead(
            Cipher::aes_256_gcm(),
            self.secret.as_slice(),
            Some(&nonce),
            &self.name,
            plain,
            &mut tag,
        )
        .ok()?;
        let mut ticket = Vec::with_capacity(NAME_BYTES + NONCE_BYTES + encrypted.len() + TAG_BYTES);
        ticket.extend_from_slice(&self.name);
        ticket.extend_from_slice(&nonce);
        ticket.extend_from_slice(&encrypted);
        ticket.extend_from_slice(&tag);
        Some(ticket)
    }

    fn open(&self, body: &[u8]) -> Option<Vec<u8>> {
        let (nonce, rest) = body.split_at_checked(NONCE_BYTES)?;
        let (encrypted, tag) = rest.split_at_checked(rest.len().checked_sub(TAG_BYTES)?)?;
        decrypt_aead(
            Cipher::aes_256_gcm(),
            self.secret.as_slice(),
            Some(nonce),
            &self.name,
            encrypted,
            tag,
        )
        .ok()
    }
}

impl TicketKeys {
    fn new() -> Self {
        Self {
            rotation: Mutex::new(Rotation {
                current: Key::generate(),
                previous: None,
                switch_at: unix_seconds() + u64::from(LIFETIME_SECONDS),
            }),
        }
    }

    fn rotation(&self) -> std::sync::MutexGuard<'_, Rotation> {
        let mut rotation = self.rotation.lock().unwrap_or_else(PoisonError::into_inner);
        let now = unix_seconds();
        if now >= rotation.switch_at {
            let replaced = std::mem::replace(&mut rotation.current, Key::generate());
            rotation.previous = Some(replaced);
            rotation.switch_at = now + u64::from(LIFETIME_SECONDS);
        }
        rotation
    }

    pub(crate) fn for_profile(profile: &str) -> Arc<Self> {
        let mut ring = RING.lock().unwrap_or_else(PoisonError::into_inner);
        ring.retain(|_, keys| keys.strong_count() > 0);
        if let Some(keys) = ring.get(profile).and_then(Weak::upgrade) {
            return keys;
        }
        let keys = Arc::new(Self::new());
        ring.insert(profile.to_owned(), Arc::downgrade(&keys));
        keys
    }

    pub(crate) fn export_all() -> Vec<TicketKeysState> {
        let ring = RING.lock().unwrap_or_else(PoisonError::into_inner);
        ring.iter()
            .filter_map(|(profile, keys)| Some((profile, keys.upgrade()?)))
            .map(|(profile, keys)| {
                let rotation = keys.rotation();
                TicketKeysState {
                    profile: profile.clone(),
                    current: rotation.current.export(),
                    previous: rotation.previous.as_ref().map(Key::export),
                    switch_at: rotation.switch_at,
                }
            })
            .collect()
    }

    pub(crate) fn import_all(states: &[TicketKeysState]) {
        let ring = RING.lock().unwrap_or_else(PoisonError::into_inner);
        for state in states {
            let Some(keys) = ring.get(&state.profile).and_then(Weak::upgrade) else {
                continue;
            };
            let Some(current) = Key::import(&state.current) else {
                log::error!(
                    "TLS ticket keys of profile `{}` are malformed",
                    state.profile
                );
                continue;
            };
            let mut rotation = keys.rotation.lock().unwrap_or_else(PoisonError::into_inner);
            rotation.current = current;
            rotation.previous = state.previous.as_ref().and_then(Key::import);
            rotation.switch_at = state.switch_at;
        }
    }
}

impl std::fmt::Debug for TicketKeys {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TicketKeys(..)")
    }
}

impl ProducesTickets for TicketKeys {
    fn enabled(&self) -> bool {
        true
    }

    fn lifetime(&self) -> u32 {
        LIFETIME_SECONDS
    }

    fn encrypt(&self, plain: &[u8]) -> Option<Vec<u8>> {
        self.rotation().current.seal(plain)
    }

    fn decrypt(&self, ticket: &[u8]) -> Option<Vec<u8>> {
        let (name, body) = ticket.split_at_checked(NAME_BYTES)?;
        let rotation = self.rotation();
        let key = [Some(&rotation.current), rotation.previous.as_ref()]
            .into_iter()
            .flatten()
            .find(|key| key.name.as_slice() == name)?;
        key.open(body)
    }
}

#[cfg(test)]
mod tests;
