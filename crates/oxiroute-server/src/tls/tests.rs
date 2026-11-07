#![allow(dead_code, unused_imports)]

#[path = "../encoding.rs"]
mod encoding;
#[path = "mod.rs"]
mod tls;

use std::{
    error::Error as _,
    fs,
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, Barrier},
    thread,
};

use openssl::{
    asn1::{Asn1Integer, Asn1Time},
    bn::BigNum,
    dh::Dh,
    dsa::Dsa,
    ec::{EcGroup, EcKey},
    hash::MessageDigest,
    nid::Nid,
    pkey::{PKey, Private},
    rsa::Rsa,
    ssl::{Ssl, SslContextBuilder, SslMethod, SslOptions, SslSessionCacheMode},
    x509::{
        X509, X509NameBuilder,
        extension::{
            AuthorityKeyIdentifier, BasicConstraints, ExtendedKeyUsage, KeyUsage,
            SubjectAlternativeName, SubjectKeyIdentifier,
        },
    },
};
use oxiroute_config::{
    AcmeChallengeType, AcmeKeyType, AlpnProtocol, Certificate, CertificateSource, ConfigDraft,
    HttpVersion, HttpVersionPolicy, SelfSignedKeyType, TlsClientAuthMode, TlsClientAuthPolicy,
    TlsPolicy, TlsProfile, TlsSessionCache, TlsVersion, UpstreamAlgorithm, UpstreamEndpoint,
    UpstreamPool, UpstreamTls, ValidatedConfig,
};
use pingora::{listeners::ALPN, upstreams::peer::HttpPeer};
use tempfile::TempDir;

use tls::{
    ActiveCertificateGeneration, CertbotActivationDirection, CertbotCandidate, CertbotLineage,
    CertbotReconcileError, CertbotReconcileOutcome, CertbotReconciler, CertbotReconcilerStatus,
    CertbotWatcherConfig, CertbotWatcherError, CertbotWatcherMonitor, CertbotWatcherSupervisor,
    CertificateGeneration, CertificateMetadata, CertificatePublishError, CertificateValidity,
    FileReconcileError, FileReconcileOutcome, FileWatcherConfig, FileWatcherSupervisor,
    MAX_CA_CERTIFICATE_BYTES, MAX_CERTIFICATE_CHAIN_BYTES, MAX_DH_PARAMETERS_BYTES,
    MAX_PRIVATE_KEY_BYTES, TlsBuildError, TlsProfilePlan, UpstreamTlsPlan,
    prepare_tls as prepare_validated_tls, prepare_upstream_tls,
};

struct IdentityFiles {
    chain: PathBuf,
    key: PathBuf,
    ca: PathBuf,
}

fn prepare_tls(config: &ValidatedConfig) -> Result<tls::PreparedTls, TlsBuildError> {
    prepare_validated_tls(config)
}

#[path = "tests/certbot_load.rs"]
mod certbot_load;
#[path = "tests/certbot_reconcile.rs"]
mod certbot_reconcile;
#[path = "tests/certbot_watcher.rs"]
mod certbot_watcher;
#[path = "tests/chain.rs"]
mod chain;
#[path = "tests/file_reconcile.rs"]
mod file_reconcile;
#[path = "tests/material.rs"]
mod material;
#[path = "tests/profile.rs"]
mod profile;
#[path = "tests/publication.rs"]
mod publication;
#[path = "tests/support.rs"]
mod support;
#[path = "tests/upstream_ca.rs"]
mod upstream_ca;

#[allow(clippy::wildcard_imports)]
use support::*;
