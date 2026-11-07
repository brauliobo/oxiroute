#[allow(clippy::wildcard_imports)]
use super::*;

const MAX_HTPASSWD_FILE_BYTES: usize = 1024 * 1024;
const MAX_BASIC_CREDENTIAL_BYTES: usize = 2048;
const MAX_BASIC_USERNAME_BYTES: usize = 256;
const MAX_BASIC_HASH_BYTES: usize = 60;
const MAX_CONCURRENT_BASIC_VERIFICATIONS: usize = 4;
const MIN_BASIC_BCRYPT_COST: u32 = 4;
const MAX_BASIC_BCRYPT_COST: u32 = 12;
const APR1_PREFIX: &str = "$apr1$";
const MAX_APR1_SALT_BYTES: usize = 8;
const APR1_DIGEST_BYTES: usize = 22;
const APR1_ROUNDS: usize = 1_000;
const APR1_ALPHABET: &[u8; 64] =
    b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

pub(crate) struct BearerTokenAccess {
    token: SecureBearerToken,
    header_name: HeaderName,
    challenge: HeaderValue,
}

#[derive(Debug)]
pub(crate) enum RouteAccess {
    Bearer(BearerTokenAccess),
    Basic(BasicHtpasswdAccess),
}

impl RouteAccess {
    pub(crate) fn load(policy: &HttpAccessPolicy) -> Result<Self, AccessPreflightError> {
        match policy {
            HttpAccessPolicy::BearerTokenFile { .. } => {
                BearerTokenAccess::load(policy).map(Self::Bearer)
            }
            HttpAccessPolicy::BasicHtpasswdFile {
                htpasswd_file_path,
                realm,
            } => BasicHtpasswdAccess::load(htpasswd_file_path, realm).map(Self::Basic),
        }
    }

    pub(crate) async fn authorizes(&self, headers: &HeaderMap) -> bool {
        match self {
            Self::Bearer(access) => access.authorizes(headers),
            Self::Basic(access) => access.authorizes(headers).await,
        }
    }

    pub(crate) fn challenge(&self) -> &HeaderValue {
        match self {
            Self::Bearer(access) => access.challenge(),
            Self::Basic(access) => &access.challenge,
        }
    }
}

pub(crate) struct BasicHtpasswdAccess {
    challenge: HeaderValue,
    dummy_hashes: Box<[BasicPasswordHash]>,
    username_case_sensitive: bool,
    users: Box<[BasicUser]>,
}

type BasicUser = ([u8; 32], Arc<str>, BasicPasswordHash);

#[derive(Clone)]
enum BasicPasswordHash {
    Bcrypt { hash: String, cost: u32 },
    Apr1(Apr1Hash),
}

#[derive(Clone)]
struct Apr1Hash {
    salt: Box<[u8]>,
    digest: [u8; APR1_DIGEST_BYTES],
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum BasicHashScheme {
    Bcrypt(u32),
    Apr1,
}

impl std::fmt::Debug for BasicHtpasswdAccess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BasicHtpasswdAccess")
            .field("challenge", &self.challenge)
            .field("user_count", &self.users.len())
            .finish_non_exhaustive()
    }
}

impl BasicHtpasswdAccess {
    pub(crate) fn load(path: &Path, realm: &str) -> Result<Self, AccessPreflightError> {
        Self::load_with_username_case(path, realm, true)
    }

    pub(crate) fn load_with_username_case(
        path: &Path,
        realm: &str,
        username_case_sensitive: bool,
    ) -> Result<Self, AccessPreflightError> {
        let bytes = read_secret_file(path, MAX_HTPASSWD_FILE_BYTES)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| AccessPreflightError)?;
        let mut users = Vec::new();
        let mut username_digests = HashSet::new();
        let mut schemes = BTreeSet::new();
        for line in text.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (username, hash) = line.split_once(':').ok_or(AccessPreflightError)?;
            if username.is_empty()
                || username.len() > MAX_BASIC_USERNAME_BYTES
                || username.bytes().any(|byte| byte.is_ascii_control())
                || hash.len() > MAX_BASIC_HASH_BYTES
            {
                return Err(AccessPreflightError);
            }
            let username_digest = sha256(username.as_bytes());
            if !username_digests.insert(username_digest) {
                return Err(AccessPreflightError);
            }
            let (parsed_hash, scheme) = BasicPasswordHash::parse(hash)?;
            schemes.insert(scheme);
            users.push((username_digest, Arc::<str>::from(username), parsed_hash));
        }
        if users.is_empty() {
            return Err(AccessPreflightError);
        }
        let dummy_hashes = schemes
            .into_iter()
            .map(BasicPasswordHash::dummy)
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice();
        Ok(Self {
            challenge: HeaderValue::from_str(&format!(
                "Basic realm=\"{realm}\", charset=\"UTF-8\""
            ))
            .expect("validated Basic realm"),
            dummy_hashes,
            username_case_sensitive,
            users: users.into_boxed_slice(),
        })
    }

    pub(super) async fn authorizes(&self, headers: &HeaderMap) -> bool {
        let mut values = headers.get_all(AUTHORIZATION).iter();
        let Some(value) = values.next() else {
            return false;
        };
        if values.next().is_some() {
            return false;
        }
        self.authenticate(value.as_bytes()).await.is_some()
    }

    pub(crate) async fn authenticate(&self, bytes: &[u8]) -> Option<Arc<str>> {
        let encoded = bytes.get(6..).filter(|_| {
            bytes
                .get(..5)
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case(b"basic"))
                && bytes.get(5) == Some(&b' ')
        })?;
        if encoded.len() > MAX_BASIC_CREDENTIAL_BYTES * 2 {
            return None;
        }
        let Ok(decoded) = STANDARD.decode(encoded) else {
            return None;
        };
        let decoded = Zeroizing::new(decoded);
        if decoded.len() > MAX_BASIC_CREDENTIAL_BYTES {
            return None;
        }
        let separator = decoded.iter().position(|byte| *byte == b':')?;
        if separator > MAX_BASIC_USERNAME_BYTES {
            return None;
        }
        let Ok(username) = std::str::from_utf8(&decoded[..separator]) else {
            return None;
        };
        let Ok(password) = std::str::from_utf8(&decoded[separator + 1..]) else {
            return None;
        };
        let username_digest = basic_client_username_digest(username, self.username_case_sensitive);
        let mut selected_index = self.users.len();
        // Complete every comparison before selecting a hash so entry position does not alter scan work.
        for (index, (candidate_digest, _, _)) in self.users.iter().enumerate() {
            let matched = usize::from(memcmp::eq(&username_digest, candidate_digest));
            let mask = 0usize.wrapping_sub(matched);
            selected_index = (selected_index & !mask) | (index & mask);
        }
        let known_user = selected_index != self.users.len();
        let selected_hash = known_user.then(|| self.users[selected_index].2.clone());
        let hashes = self
            .dummy_hashes
            .iter()
            .map(|dummy| {
                selected_hash
                    .as_ref()
                    .filter(|hash| hash.scheme() == dummy.scheme())
                    .unwrap_or(dummy)
                    .clone()
            })
            .collect::<Box<[_]>>();
        let password = Zeroizing::new(password.to_owned());
        let semaphore = basic_auth_semaphore();
        let Ok(permit) = semaphore.try_acquire_owned() else {
            return None;
        };
        let verified = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            hashes.iter().any(|hash| hash.verify(password.as_bytes()))
        })
        .await
        .is_ok_and(|verified| known_user && verified);
        verified.then(|| Arc::clone(&self.users[selected_index].1))
    }
}

fn basic_client_username_digest(username: &str, case_sensitive: bool) -> [u8; 32] {
    if case_sensitive {
        sha256(username.as_bytes())
    } else {
        sha256(username.to_ascii_lowercase().as_bytes())
    }
}

impl BasicPasswordHash {
    fn parse(hash: &str) -> Result<(Self, BasicHashScheme), AccessPreflightError> {
        if matches!(hash.get(..4), Some("$2y$" | "$2b$" | "$2a$")) {
            if hash.len() != MAX_BASIC_HASH_BYTES {
                return Err(AccessPreflightError);
            }
            let parts = hash
                .parse::<bcrypt::HashParts>()
                .map_err(|_| AccessPreflightError)?;
            let cost = parts.get_cost();
            if !(MIN_BASIC_BCRYPT_COST..=MAX_BASIC_BCRYPT_COST).contains(&cost) {
                return Err(AccessPreflightError);
            }
            return Ok((
                Self::Bcrypt {
                    hash: hash.to_owned(),
                    cost,
                },
                BasicHashScheme::Bcrypt(cost),
            ));
        }

        let apr1 = Apr1Hash::parse(hash)?;
        Ok((Self::Apr1(apr1), BasicHashScheme::Apr1))
    }

    fn dummy(scheme: BasicHashScheme) -> Result<Self, AccessPreflightError> {
        match scheme {
            BasicHashScheme::Bcrypt(cost) => {
                let hash = bcrypt::hash_with_salt(
                    b"oxiroute-unknown-basic-user",
                    cost,
                    *b"OxiRouteDummy123",
                )
                .map_err(|_| AccessPreflightError)?
                .to_string();
                Ok(Self::Bcrypt { hash, cost })
            }
            BasicHashScheme::Apr1 => Ok(Self::Apr1(Apr1Hash {
                salt: Box::from(&b"OxiRoute"[..]),
                digest: apr1_digest(b"oxiroute-unknown-basic-user", b"OxiRoute")
                    .map_err(|_| AccessPreflightError)?,
            })),
        }
    }

    fn scheme(&self) -> BasicHashScheme {
        match self {
            Self::Bcrypt { cost, .. } => BasicHashScheme::Bcrypt(*cost),
            Self::Apr1(_) => BasicHashScheme::Apr1,
        }
    }

    fn verify(&self, password: &[u8]) -> bool {
        match self {
            Self::Bcrypt { hash, .. } => bcrypt::verify(password, hash).unwrap_or(false),
            Self::Apr1(hash) => apr1_digest(password, &hash.salt)
                .is_ok_and(|digest| memcmp::eq(&digest, &hash.digest)),
        }
    }
}

impl Apr1Hash {
    fn parse(hash: &str) -> Result<Self, AccessPreflightError> {
        let value = hash.strip_prefix(APR1_PREFIX).ok_or(AccessPreflightError)?;
        let (salt, digest) = value.split_once('$').ok_or(AccessPreflightError)?;
        if salt.is_empty()
            || salt.len() > MAX_APR1_SALT_BYTES
            || !salt.bytes().all(is_apr1_character)
            || digest.len() != APR1_DIGEST_BYTES
            || !digest.bytes().all(is_apr1_character)
            || !matches!(digest.as_bytes().last(), Some(b'.' | b'/' | b'0' | b'1'))
        {
            return Err(AccessPreflightError);
        }
        let mut parsed_digest = [0; APR1_DIGEST_BYTES];
        parsed_digest.copy_from_slice(digest.as_bytes());
        Ok(Self {
            salt: salt.as_bytes().into(),
            digest: parsed_digest,
        })
    }
}

fn is_apr1_character(byte: u8) -> bool {
    APR1_ALPHABET.contains(&byte)
}

pub(super) fn apr1_digest(
    password: &[u8],
    salt: &[u8],
) -> Result<[u8; APR1_DIGEST_BYTES], openssl::error::ErrorStack> {
    let mut alternate = Hasher::new(MessageDigest::md5())?;
    alternate.update(password)?;
    alternate.update(salt)?;
    alternate.update(password)?;
    let alternate = alternate.finish()?;

    let mut initial = Hasher::new(MessageDigest::md5())?;
    initial.update(password)?;
    initial.update(APR1_PREFIX.as_bytes())?;
    initial.update(salt)?;
    let mut remaining = password.len();
    while remaining >= alternate.len() {
        initial.update(&alternate)?;
        remaining -= alternate.len();
    }
    initial.update(&alternate[..remaining])?;
    let mut length = password.len();
    while length != 0 {
        if length & 1 == 0 {
            initial.update(&password[..1])?;
        } else {
            initial.update(&[0])?;
        }
        length >>= 1;
    }
    let mut digest = initial.finish()?;

    for round in 0..APR1_ROUNDS {
        let mut hasher = Hasher::new(MessageDigest::md5())?;
        if round & 1 == 0 {
            hasher.update(&digest)?;
        } else {
            hasher.update(password)?;
        }
        if round % 3 != 0 {
            hasher.update(salt)?;
        }
        if round % 7 != 0 {
            hasher.update(password)?;
        }
        if round & 1 == 0 {
            hasher.update(password)?;
        } else {
            hasher.update(&digest)?;
        }
        digest = hasher.finish()?;
    }

    Ok(apr1_encode(
        digest
            .as_ref()
            .try_into()
            .expect("OpenSSL MD5 digest is 16 bytes"),
    ))
}

fn apr1_encode(digest: [u8; 16]) -> [u8; APR1_DIGEST_BYTES] {
    let mut encoded = [0; APR1_DIGEST_BYTES];
    let mut offset = 0;
    for (first, second, third) in [(0, 6, 12), (1, 7, 13), (2, 8, 14), (3, 9, 15), (4, 10, 5)] {
        offset = apr1_encode_group(
            &mut encoded,
            offset,
            digest[first],
            digest[second],
            digest[third],
            4,
        );
    }
    apr1_encode_group(&mut encoded, offset, 0, 0, digest[11], 2);
    encoded
}

fn apr1_encode_group(
    output: &mut [u8; APR1_DIGEST_BYTES],
    mut offset: usize,
    first: u8,
    second: u8,
    third: u8,
    count: usize,
) -> usize {
    let mut value = (u32::from(first) << 16) | (u32::from(second) << 8) | u32::from(third);
    for _ in 0..count {
        output[offset] = APR1_ALPHABET[(value & 0x3f) as usize];
        offset += 1;
        value >>= 6;
    }
    offset
}

fn basic_auth_semaphore() -> Arc<tokio::sync::Semaphore> {
    static SEMAPHORE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    Arc::clone(SEMAPHORE.get_or_init(|| {
        Arc::new(tokio::sync::Semaphore::new(
            MAX_CONCURRENT_BASIC_VERIFICATIONS,
        ))
    }))
}

impl std::fmt::Debug for BearerTokenAccess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BearerTokenAccess")
            .field("header_name", &self.header_name)
            .field("challenge", &self.challenge)
            .finish_non_exhaustive()
    }
}

impl BearerTokenAccess {
    pub(crate) fn load(policy: &HttpAccessPolicy) -> Result<Self, AccessPreflightError> {
        let HttpAccessPolicy::BearerTokenFile {
            token_file_path,
            header_name,
            realm,
        } = policy
        else {
            return Err(AccessPreflightError);
        };
        let challenge = realm.as_ref().map_or_else(
            || HeaderValue::from_static("Bearer"),
            |realm| {
                HeaderValue::from_str(&format!("Bearer realm=\"{realm}\""))
                    .expect("validated Bearer realm")
            },
        );
        Ok(Self {
            token: SecureBearerToken::load(token_file_path).map_err(|_| AccessPreflightError)?,
            header_name: HeaderName::from_bytes(header_name.as_bytes())
                .expect("validated access header name"),
            challenge,
        })
    }

    pub(crate) fn authorizes(&self, headers: &HeaderMap) -> bool {
        matches!(
            single_header(headers, &self.header_name),
            HeaderCardinality::Single(value) if self.token.authorizes(value.as_bytes())
        )
    }

    pub(crate) fn challenge(&self) -> &HeaderValue {
        &self.challenge
    }
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("HTTP route access policy failed secure preflight")]
pub(crate) struct AccessPreflightError;
