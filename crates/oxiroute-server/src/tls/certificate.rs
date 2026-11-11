use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashSet},
    fmt,
    net::IpAddr,
    path::Path,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering as AtomicOrdering},
    },
};

use super::{
    CertificateIdentitySan, CertificateIdentitySans, MAX_CA_CERTIFICATE_BYTES,
    MAX_CERTIFICATE_CHAIN_BYTES, MAX_CERTIFICATES_IN_CHAIN, MAX_CLIENT_CA_CERTIFICATES,
    MAX_DH_PARAMETERS_BYTES, MAX_PRIVATE_KEY_BYTES, TlsAlpnChallengeIdentity,
    TlsAlpnChallengeStore, TlsBuildError, certificate_identity_sans, certificate_is_ca_capable,
    pem_labels, read_bounded_stable,
};
use crate::encoding::lower_hex;
use arc_swap::ArcSwap;
use async_trait::async_trait;
use openssl::{
    asn1::Asn1Time,
    bn::{BigNum, MsbOption},
    dh::Dh,
    ec::{EcGroup, EcKey},
    error::ErrorStack,
    hash::MessageDigest,
    nid::Nid,
    pkey::{Id, PKey, Params, Private},
    rsa::Rsa,
    sha::sha256,
    ssl::{
        NameType, SslAcceptor, SslMethod, SslOptions, SslSessionCacheMode, SslVerifyMode,
        SslVersion,
    },
    stack::Stack,
    x509::{
        X509, X509NameBuilder, X509PurposeId, X509StoreContext,
        extension::{BasicConstraints, ExtendedKeyUsage, KeyUsage, SubjectAlternativeName},
        store::X509StoreBuilder,
        verify::X509VerifyFlags,
    },
};
use oxiroute_config::{
    AlpnProtocol, SelfSignedKeyType, TlsBackend, TlsClientAuthMode, TlsKeyExchangeGroup, TlsPolicy,
    TlsProfile, TlsVersion,
};
use pingora::{
    listeners::{ALPN, TlsAccept, tls::TlsSettings},
    protocols::tls::CustomALPN,
    protocols::tls::TlsRef,
    services::listening::Service,
    tls::ext::{ssl_add_chain_cert, ssl_use_certificate, ssl_use_private_key},
    tls::ssl::{AlpnError, Ssl as PingoraSsl},
};
use x509_parser::parse_x509_certificate;

mod accept;
mod active;
mod client_auth;
mod generation;
mod profile;
mod validate;

#[allow(clippy::wildcard_imports)]
use accept::*;
pub use active::*;
#[allow(clippy::wildcard_imports)]
use client_auth::*;
pub use generation::*;
pub use profile::*;
#[allow(clippy::wildcard_imports)]
use validate::*;
