use std::sync::{Arc, LazyLock, PoisonError, RwLock};

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use openssl::{
    hash::MessageDigest,
    memcmp,
    pkey::{PKey, Private},
    rand::rand_bytes,
    sign::Signer,
};
use zeroize::Zeroizing;

use super::{media::Route as MediaRoute, vod::Route as VodRoute};

pub(super) mod playlist;

pub(super) const TICKET_PARAMETER: &str = "ticket";
pub(super) const DEFAULT_TTL_SECONDS: u32 = 90;
pub(super) const MAX_TTL_SECONDS: u32 = 600;

const SECRET_BYTES: usize = 32;
const MAC_DOMAIN: &[u8] = b"oxiroute media ticket v1\0";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Scope<'a> {
    Stream {
        service: &'a str,
        application: &'a str,
        stream: &'a str,
    },
    Vod {
        service: &'a str,
        application: &'a str,
        source: &'a str,
        path: &'a str,
    },
    Recording {
        id: &'a str,
    },
}

impl Scope<'_> {
    fn canonical(&self) -> String {
        match self {
            Self::Stream {
                service,
                application,
                stream,
            } => format!("stream:{service}/{application}/{stream}"),
            Self::Vod {
                service,
                application,
                source,
                path,
            } => format!("vod:{service}/{application}/{source}/{path}"),
            Self::Recording { id } => format!("recording:{id}"),
        }
    }
}

impl<'a> From<&MediaRoute<'a>> for Scope<'a> {
    fn from(route: &MediaRoute<'a>) -> Self {
        Self::Stream {
            service: route.service,
            application: route.application,
            stream: route.stream,
        }
    }
}

impl<'a> From<&VodRoute<'a>> for Scope<'a> {
    fn from(route: &VodRoute<'a>) -> Self {
        Self::Vod {
            service: route.service,
            application: route.application,
            source: route.source,
            path: route.path,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TicketError {
    Malformed,
    Invalid,
    Expired,
}

static PROCESS_KEY: LazyLock<Arc<TicketKey>> = LazyLock::new(|| Arc::new(TicketKey::generate()));

pub(super) struct TicketKey {
    signing: RwLock<Signing>,
}

struct Signing {
    secret: Zeroizing<[u8; SECRET_BYTES]>,
    key: PKey<Private>,
}

impl Signing {
    fn new(secret: Zeroizing<[u8; SECRET_BYTES]>) -> Self {
        let key = PKey::hmac(secret.as_slice()).expect("HMAC key from random bytes");
        Self { secret, key }
    }
}

impl TicketKey {
    pub(super) fn generate() -> Self {
        let mut secret = Zeroizing::new([0_u8; SECRET_BYTES]);
        rand_bytes(secret.as_mut_slice()).expect("OpenSSL random source");
        Self {
            signing: RwLock::new(Signing::new(secret)),
        }
    }

    pub(super) fn process() -> Arc<Self> {
        Arc::clone(&PROCESS_KEY)
    }

    fn export_secret(&self) -> String {
        let signing = self.signing.read().unwrap_or_else(PoisonError::into_inner);
        STANDARD.encode(signing.secret.as_slice())
    }

    fn import_secret(&self, secret: &str) -> bool {
        let Some(secret) = STANDARD
            .decode(secret)
            .ok()
            .and_then(|bytes| <[u8; SECRET_BYTES]>::try_from(bytes).ok())
        else {
            return false;
        };
        *self.signing.write().unwrap_or_else(PoisonError::into_inner) =
            Signing::new(Zeroizing::new(secret));
        true
    }

    pub(super) fn mint(&self, scope: &Scope<'_>, expires_at_unix_ms: u64) -> String {
        let signing = self.signing.read().unwrap_or_else(PoisonError::into_inner);
        let mut signer =
            Signer::new(MessageDigest::sha256(), &signing.key).expect("HMAC-SHA256 signer");
        signer.update(MAC_DOMAIN).expect("HMAC input");
        signer
            .update(scope.canonical().as_bytes())
            .expect("HMAC input");
        signer.update(&[0]).expect("HMAC input");
        signer
            .update(&expires_at_unix_ms.to_be_bytes())
            .expect("HMAC input");
        let mac = signer.sign_to_vec().expect("HMAC-SHA256 output");
        format!("{expires_at_unix_ms}.{}", URL_SAFE_NO_PAD.encode(mac))
    }

    pub(super) fn verify(
        &self,
        scope: &Scope<'_>,
        ticket: &str,
        now_unix_ms: u64,
    ) -> Result<(), TicketError> {
        let expires_at_unix_ms = ticket
            .split_once('.')
            .and_then(|(expiry, _)| expiry.parse::<u64>().ok())
            .ok_or(TicketError::Malformed)?;
        let expected = self.mint(scope, expires_at_unix_ms);
        if expected.len() != ticket.len() || !memcmp::eq(expected.as_bytes(), ticket.as_bytes()) {
            return Err(TicketError::Invalid);
        }
        if now_unix_ms >= expires_at_unix_ms {
            return Err(TicketError::Expired);
        }
        Ok(())
    }
}

pub(crate) fn export_media_ticket_secret() -> String {
    PROCESS_KEY.export_secret()
}

pub(crate) fn import_media_ticket_secret(secret: &str) -> bool {
    PROCESS_KEY.import_secret(secret)
}

pub(super) fn query_ticket(query: Option<&str>) -> Result<Option<&str>, TicketError> {
    let mut tickets = query
        .into_iter()
        .flat_map(|query| query.split('&'))
        .filter_map(|pair| pair.strip_prefix(TICKET_PARAMETER)?.strip_prefix('='));
    let ticket = tickets.next();
    if tickets.next().is_some() {
        return Err(TicketError::Malformed);
    }
    Ok(ticket)
}

pub(super) fn ticket_path(object_path: &str, ticket: &str) -> String {
    format!("{object_path}?{TICKET_PARAMETER}={ticket}")
}

#[cfg(test)]
mod tests;
