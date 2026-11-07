use std::{
    collections::BTreeSet,
    io,
    path::Path,
    sync::{
        Arc, Mutex, TryLockError,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use openssl::{
    asn1::{Asn1Time, Asn1TimeRef},
    x509::X509,
};
use oxiroute_acme::{
    Account, AccountKey, AccountKeyAlgorithm, AcmeClient, AcmeError, AcmeOperation, AcmeStateError,
    AcmeTransport, AuthorizationStatus, CertificateMaterial, ChallengeRecord, ChallengeStore,
    ChallengeStoreError, ChallengeType, Dns01Cancellation, Dns01Challenge, Dns01Credentials,
    Dns01Operation, Dns01Provider, Dns01ProviderError, JobStatus, LeafKeyAlgorithm,
    MAX_DNS01_CREDENTIAL_BYTES, MAX_JOB_BYTES, OriginPolicy, PollPolicy, RedactedOutcome,
    RenewalInformation, RevisionMetadata, RevisionStore, SecretBytes, StateStore,
    SystemAcmeTransport, SystemClock, renewal_due, stable_renewal_time,
    stable_renewal_time_in_window,
};
use oxiroute_config::{AcmeChallengeType, AcmeDns01Config, AcmeKeyType, SelfSignedKeyType};
use serde::{Deserialize, Serialize};

use super::{
    ActiveCertificateGeneration, CertificateGeneration, MAX_CERTIFICATE_CHAIN_BYTES,
    TlsAlpnChallenge, TlsAlpnChallengeStore, TlsBuildError,
};

mod actions;
mod construct;
mod dns01;
mod helpers;
mod job_state;
mod model;
mod renew;

pub use model::*;

#[allow(clippy::wildcard_imports)]
use helpers::*;

#[cfg(test)]
mod tests;
