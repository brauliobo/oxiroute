use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    path::{Path, PathBuf},
    sync::Arc,
};

use oxiroute_acme::{ChallengeStore, Dns01ProviderRegistry, StateStore};
use oxiroute_config::{CertificateSource, ValidatedConfig};
#[cfg(unix)]
use rustix::fs::{self as rustix_fs, Mode, OFlags};
use zeroize::Zeroizing;

mod acme;
mod certbot;
mod certificate;
mod file;
pub(crate) mod ticket_keys;
mod tls_alpn;
mod upstream;
mod watcher_engine;

pub use acme::{
    AcmeDnsCleanupRecovery, AcmeManagedError, AcmeManagedOutcome, AcmeManagedPolicy,
    AcmeManagedReconciler, AcmeManagedStatus,
};
pub use certbot::reconcile::{
    CertbotActivationDirection, CertbotReconcileError, CertbotReconcileOutcome, CertbotReconciler,
    CertbotReconcilerStatus,
};
pub use certbot::watcher::{
    CertbotWatcherConfig, CertbotWatcherError, CertbotWatcherMonitor, CertbotWatcherStatus,
    CertbotWatcherSupervisor,
};
pub use certbot::{CertbotCandidate, CertbotLineage};
pub(crate) use certificate::CompiledTlsProfilePolicy;
pub use certificate::{
    ActiveCertificateGeneration, CertificateGeneration, CertificateMetadata,
    CertificatePublishError, CertificateValidity, TlsProfilePlan,
};
pub use file::reconcile::{
    FileReconcileError, FileReconcileOutcome, FileReconciler, FileReconcilerStatus,
};
pub use file::watcher::{
    FileWatcherConfig, FileWatcherError, FileWatcherMonitor, FileWatcherStatus,
    FileWatcherSupervisor,
};
pub use tls_alpn::{
    TLS_ALPN_IDENTIFIER_OID, TLS_ALPN_PROTOCOL, TlsAlpnChallenge, TlsAlpnChallengeError,
    TlsAlpnChallengeIdentity, TlsAlpnChallengeLease, TlsAlpnChallengeStore,
};
pub(crate) use upstream::UpstreamTlsBlueprint;
pub use upstream::{UpstreamTlsPlan, prepare_upstream_tls};

mod error;
mod files;
mod prepare;
pub(crate) mod provider;
mod x509;

pub use error::*;
pub(crate) use files::*;
pub use prepare::*;
pub(crate) use x509::*;
