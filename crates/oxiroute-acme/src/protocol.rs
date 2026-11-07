use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
    net::IpAddr,
    sync::Arc,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use openssl::{
    bn::BigNumContext,
    ec::{EcGroup, EcKey, PointConversionForm},
    ecdsa::EcdsaSig,
    error::ErrorStack,
    hash::MessageDigest,
    nid::Nid,
    pkey::{Id, PKey, Private},
    rsa::Rsa,
    sign::Signer,
    stack::Stack,
    x509::{X509, X509NameBuilder, X509Req, X509ReqBuilder, extension::SubjectAlternativeName},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{Clock, Dns01Cancellation, Dns01Challenge, SecretBytes};

mod polling;

use polling::{PollAttempt, poll_acme};
#[cfg(test)]
use polling::{bounded_poll_policy, jittered_delay, retry_after};

mod account;
mod client;
mod directory;
mod error;
mod http;
mod order;
mod response;

pub use account::*;
pub use client::*;
pub use directory::*;
pub use error::*;
pub use http::*;
pub use order::*;
#[allow(clippy::wildcard_imports)]
use response::*;

pub const MAX_ACME_BODY_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_ACME_URL_BYTES: usize = 2_048;
pub const MAX_NONCES: usize = 32;
pub const MAX_IDENTIFIERS: usize = 100;
pub const MAX_POLL_ATTEMPTS: usize = 64;
pub const MAX_POLL_DELAY_SECONDS: u64 = 300;
pub const MAX_RENEWAL_INFORMATION_WINDOW_SECONDS: u64 =
    crate::clock::MAX_RENEWAL_INFORMATION_WINDOW_SECONDS;

#[cfg(test)]
mod tests;
