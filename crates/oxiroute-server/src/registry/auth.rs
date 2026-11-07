use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use arc_swap::ArcSwap;
use openssl::{memcmp, rand::rand_bytes, sha::sha256};

use super::error::RegistryError;

const SECRET_BYTES: usize = 32;

/// What a credential may do. The root management token is `Admin`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Scope {
    Read,
    Register {
        pools: Vec<String>,
        name_prefix: Option<String>,
    },
    Admin,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Principal {
    pub id: String,
    pub scope: Scope,
}

impl Principal {
    pub fn admin(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            scope: Scope::Admin,
        }
    }

    pub fn can_read(&self) -> bool {
        !matches!(self.scope, Scope::Register { .. })
    }

    pub fn check_register(&self, pool: &str, name: &str) -> Result<(), RegistryError> {
        match &self.scope {
            Scope::Admin => Ok(()),
            Scope::Register { pools, name_prefix }
                if pools.iter().any(|allowed| allowed == pool)
                    && name_prefix
                        .as_deref()
                        .is_none_or(|prefix| name.starts_with(prefix)) =>
            {
                Ok(())
            }
            Scope::Read | Scope::Register { .. } => Err(RegistryError::ScopeDenied),
        }
    }

    pub fn check_read(&self) -> Result<(), RegistryError> {
        if self.can_read() {
            Ok(())
        } else {
            Err(RegistryError::ScopeDenied)
        }
    }

    pub fn check_admin(&self) -> Result<(), RegistryError> {
        if self.scope == Scope::Admin {
            Ok(())
        } else {
            Err(RegistryError::ScopeDenied)
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TokenFileError {
    #[error("token file is not accessible: {0}")]
    Io(#[from] std::io::Error),
    #[error("token file mode must be 0600 or 0640 and not writable by others")]
    InsecureMode,
    #[error("token file line {0} is not `<id> <sha256-hex> <scope>`")]
    InvalidLine(usize),
}

struct TokenEntry {
    principal: Principal,
    digest: [u8; 32],
}

/// Scoped tokens, stored only as SHA-256 of 256-bit random secrets.
#[derive(Default)]
pub struct TokenFile {
    entries: Vec<TokenEntry>,
}

impl TokenFile {
    pub fn parse(text: &str) -> Result<Self, TokenFileError> {
        let mut entries: Vec<TokenEntry> = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let invalid = || TokenFileError::InvalidLine(index + 1);
            let mut fields = line.splitn(3, char::is_whitespace);
            let (id, digest, scope) = (fields.next(), fields.next(), fields.next());
            let (Some(id), Some(digest), Some(scope)) = (id, digest, scope) else {
                return Err(invalid());
            };
            if entries.iter().any(|entry| entry.principal.id == id) {
                return Err(invalid());
            }
            entries.push(TokenEntry {
                principal: Principal {
                    id: id.to_owned(),
                    scope: parse_scope(scope.trim()).ok_or_else(invalid)?,
                },
                digest: parse_digest(digest).ok_or_else(invalid)?,
            });
        }
        Ok(Self { entries })
    }

    pub fn load(path: &Path) -> Result<Self, TokenFileError> {
        let mode = fs::metadata(path)?.permissions().mode();
        if mode & 0o037 != 0 {
            return Err(TokenFileError::InsecureMode);
        }
        Self::parse(&fs::read_to_string(path)?)
    }

    /// Compares against every entry so timing does not reveal which token matched.
    pub fn authenticate(&self, secret: &[u8]) -> Option<Principal> {
        let digest = sha256(secret);
        self.entries
            .iter()
            .fold(None, |found, entry| {
                if memcmp::eq(&entry.digest, &digest) {
                    Some(&entry.principal)
                } else {
                    found
                }
            })
            .cloned()
    }
}

fn parse_scope(scope: &str) -> Option<Scope> {
    let mut words = scope.split_whitespace();
    match words.next()? {
        "read" => words.next().is_none().then_some(Scope::Read),
        "register" => {
            let (mut pools, mut name_prefix) = (Vec::new(), None);
            for word in words {
                match word.split_once('=')? {
                    ("pools", list) => pools = list.split(',').map(str::to_owned).collect(),
                    ("name_prefix", prefix) => name_prefix = Some(prefix.to_owned()),
                    _ => return None,
                }
            }
            (!pools.is_empty() && pools.iter().all(|pool| !pool.is_empty()))
                .then_some(Scope::Register { pools, name_prefix })
        }
        _ => None,
    }
}

fn parse_digest(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut digest = [0_u8; 32];
    for (byte, pair) in digest.iter_mut().zip(hex.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(digest)
}

/// Creates a secret and the token-file line that authorizes it. Show the secret once.
pub fn mint_token(id: &str, scope: &str) -> Result<(String, String), openssl::error::ErrorStack> {
    let mut secret = [0_u8; SECRET_BYTES];
    rand_bytes(&mut secret)?;
    let secret = hex(&secret);
    let line = format!("{id} {} {scope}", hex(&sha256(secret.as_bytes())));
    Ok((secret, line))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The token file, reloaded atomically when it changes on disk. A broken edit keeps the last good set.
pub struct TokenStore {
    path: PathBuf,
    current: ArcSwap<TokenFile>,
    seen: Mutex<Option<(u64, i64, u64)>>,
}

impl TokenStore {
    pub fn open(path: &Path) -> Result<Arc<Self>, TokenFileError> {
        let store = Self {
            path: path.to_owned(),
            current: ArcSwap::from_pointee(TokenFile::load(path)?),
            seen: Mutex::new(None),
        };
        store.reload_if_changed();
        Ok(Arc::new(store))
    }

    pub fn authenticate(&self, secret: &[u8]) -> Option<Principal> {
        self.reload_if_changed();
        self.current.load().authenticate(secret)
    }

    fn reload_if_changed(&self) {
        let Ok(metadata) = fs::metadata(&self.path) else {
            return;
        };
        let stamp = (
            metadata.ino(),
            metadata.mtime_nsec() + metadata.mtime() * 1_000_000_000,
            metadata.len(),
        );
        let mut seen = self
            .seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *seen == Some(stamp) {
            return;
        }
        *seen = Some(stamp);
        match TokenFile::load(&self.path) {
            Ok(file) => self.current.store(Arc::new(file)),
            Err(error) => log::error!("registry token file not reloaded: {error}"),
        }
    }
}

#[cfg(test)]
mod tests;
