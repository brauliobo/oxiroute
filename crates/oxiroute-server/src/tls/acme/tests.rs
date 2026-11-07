use std::{
    collections::VecDeque,
    fs,
    sync::{
        Arc, Barrier, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use openssl::{
    asn1::Asn1Time,
    bn::{BigNum, MsbOption},
    ec::{EcGroup, EcKey},
    hash::MessageDigest,
    nid::Nid,
    pkey::{PKey, Private},
    x509::{
        X509, X509NameBuilder, X509Req,
        extension::{
            AuthorityKeyIdentifier, BasicConstraints, ExtendedKeyUsage, KeyUsage,
            SubjectAlternativeName, SubjectKeyIdentifier,
        },
    },
};
use oxiroute_acme::{
    AcmeOperation, AcmeTransport, ChallengeStore, Dns01Cancellation, Dns01Challenge,
    Dns01Credentials, Dns01Operation, Dns01Provider, Dns01ProviderError, Dns01Record, HttpRequest,
    HttpResponse, JobState, RevisionStore, StateStore, TransportError,
};
use oxiroute_config::{AcmeDns01Config, AcmeKeyType, SelfSignedKeyType};
use serde_json::Value;
use tempfile::TempDir;

use super::*;

mod dns01;
mod issuance;
mod lifecycle;
mod support;

#[allow(clippy::wildcard_imports)]
use support::*;
