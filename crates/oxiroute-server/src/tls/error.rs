#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Debug, thiserror::Error)]
pub enum TlsBuildError {
    #[error("managed ACME certificate `{certificate}` state is unavailable or invalid")]
    AcmeState {
        certificate: String,
        #[source]
        source: Box<oxiroute_acme::AcmeStateError>,
    },
    #[error(
        "managed ACME certificate `{certificate}` uses unsupported DNS-01 provider `{provider}`"
    )]
    DnsProviderUnsupported {
        certificate: String,
        provider: String,
    },
    #[error("managed ACME certificate `{certificate}` could not be published")]
    AcmePublication {
        certificate: String,
        #[source]
        source: CertificatePublishError,
    },
    #[error("failed to open {kind} file `{path}` for `{owner}`")]
    FileOpen {
        owner: String,
        kind: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to inspect {kind} file `{path}` for `{owner}`")]
    FileMetadata {
        owner: String,
        kind: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{kind} path `{path}` for `{owner}` is not a regular file")]
    NotRegularFile {
        owner: String,
        kind: &'static str,
        path: PathBuf,
    },
    #[error("{kind} file `{path}` for `{owner}` exceeds the {limit}-byte limit")]
    FileTooLarge {
        owner: String,
        kind: &'static str,
        path: PathBuf,
        limit: usize,
    },
    #[error("failed to read {kind} file `{path}` for `{owner}`")]
    FileRead {
        owner: String,
        kind: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{kind} file `{path}` for `{owner}` changed while it was read")]
    FileChanged {
        owner: String,
        kind: &'static str,
        path: PathBuf,
    },
    #[error("development certificate `{certificate}` validity is outside the supported bounds")]
    InvalidSelfSignedValidity { certificate: String },
    #[error("failed to generate the development certificate key for `{certificate}`")]
    SelfSignedKeyGeneration {
        certificate: String,
        #[source]
        source: openssl::error::ErrorStack,
    },
    #[error("failed to generate the development certificate for `{certificate}`")]
    SelfSignedCertificateGeneration {
        certificate: String,
        #[source]
        source: openssl::error::ErrorStack,
    },
    #[error("development certificate `{certificate}` is not self-signed")]
    SelfSignedSignatureMismatch { certificate: String },
    #[error("{kind} file `{path}` for `{owner}` is empty")]
    EmptyFile {
        owner: String,
        kind: &'static str,
        path: PathBuf,
    },
    #[error("failed to resolve Certbot {kind} directory `{path}` for `{certificate}`")]
    CertbotDirectoryCanonicalization {
        certificate: String,
        kind: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("Certbot {kind} path `{path}` for `{certificate}` is not a directory")]
    CertbotPathNotDirectory {
        certificate: String,
        kind: &'static str,
        path: PathBuf,
    },
    #[error("Certbot live and archive directories for `{certificate}` resolve to `{path}`")]
    DuplicateCertbotDirectories { certificate: String, path: PathBuf },
    #[error("failed to inspect Certbot live link `{path}` for `{certificate}`")]
    CertbotLiveLinkMetadata {
        certificate: String,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("Certbot live entry `{path}` for `{certificate}` is not a symlink")]
    CertbotLiveEntryNotSymlink { certificate: String, path: PathBuf },
    #[error("failed to read Certbot live link `{path}` for `{certificate}`")]
    CertbotLiveLinkRead {
        certificate: String,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("Certbot live link `{path}` for `{certificate}` has invalid target `{target}`")]
    InvalidCertbotLiveLinkTarget {
        certificate: String,
        path: PathBuf,
        target: PathBuf,
    },
    #[error("Certbot live links for `{certificate}` mix archive revisions {expected} and {found}")]
    MixedCertbotArchiveRevisions {
        certificate: String,
        expected: u64,
        found: u64,
    },
    #[error("failed to inspect {kind} archive entry `{path}` for `{certificate}`")]
    CertbotArchiveEntryMetadata {
        certificate: String,
        kind: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{kind} archive entry `{path}` for `{certificate}` is not a regular file")]
    CertbotArchiveEntryNotRegular {
        certificate: String,
        kind: &'static str,
        path: PathBuf,
    },
    #[error("failed to read Certbot archive private-key link `{path}` for `{certificate}`")]
    CertbotArchivePrivateKeyLinkRead {
        certificate: String,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "Certbot archive private-key link `{path}` for `{certificate}` has invalid target `{target}`"
    )]
    InvalidCertbotArchivePrivateKeyLink {
        certificate: String,
        path: PathBuf,
        target: PathBuf,
    },
    #[error("Certbot full chain `{path}` for `{certificate}` does not equal cert.pem + chain.pem")]
    CertbotFullchainMismatch { certificate: String, path: PathBuf },
    #[error("Certbot lineage for `{certificate}` changed while it was read")]
    CertbotLineageChanged { certificate: String },
    #[error(
        "private key file `{path}` for `{certificate}` must use mode 0400, 0600, 0440, or 0640"
    )]
    InsecurePrivateKeyPermissions { certificate: String, path: PathBuf },
    #[error("{kind} file `{path}` for `{owner}` has an invalid PEM envelope: {detail}")]
    InvalidPem {
        owner: String,
        kind: &'static str,
        path: PathBuf,
        detail: &'static str,
    },
    #[error(
        "certificate chain for `{certificate}` contains {count} certificates; maximum is {MAX_CERTIFICATES_IN_CHAIN}"
    )]
    TooManyChainCertificates { certificate: String, count: usize },
    #[error("failed to parse certificate chain `{path}` for `{certificate}`")]
    CertificateParse {
        certificate: String,
        path: PathBuf,
        #[source]
        source: openssl::error::ErrorStack,
    },
    #[error(
        "private key file `{path}` for `{certificate}` must contain one unencrypted private key"
    )]
    EncryptedPrivateKey { certificate: String, path: PathBuf },
    #[error("private key file `{path}` for `{certificate}` must contain exactly one private key")]
    PrivateKeyCount { certificate: String, path: PathBuf },
    #[error("failed to parse private key `{path}` for `{certificate}`")]
    PrivateKeyParse {
        certificate: String,
        path: PathBuf,
        #[source]
        source: openssl::error::ErrorStack,
    },
    #[error("failed to read the leaf public key for certificate `{certificate}`")]
    LeafPublicKey {
        certificate: String,
        #[source]
        source: openssl::error::ErrorStack,
    },
    #[error("leaf certificate and private key do not match for `{certificate}`")]
    PrivateKeyMismatch { certificate: String },
    #[error("certificate `{certificate}` must use an RSA or EC private key")]
    UnsupportedPrivateKeyAlgorithm { certificate: String },
    #[error(
        "certificate `{certificate}` private key has {bits} bits; minimum for its algorithm is {minimum_bits}"
    )]
    PrivateKeyTooWeak {
        certificate: String,
        bits: u32,
        minimum_bits: u32,
    },
    #[error("failed to encode certificate `{certificate}` for key-usage validation")]
    CertificateKeyUsageEncoding {
        certificate: String,
        #[source]
        source: openssl::error::ErrorStack,
    },
    #[error("failed to inspect key usage for certificate `{certificate}`")]
    CertificateKeyUsageInspection { certificate: String },
    #[error("certificate `{certificate}` key usage must permit digital signatures")]
    MissingDigitalSignatureKeyUsage { certificate: String },
    #[error("certificate `{certificate}` is not currently valid (not before {not_before})")]
    CertificateNotYetValid {
        certificate: String,
        not_before: String,
    },
    #[error("certificate `{certificate}` is not currently valid (expired at {not_after})")]
    CertificateExpired {
        certificate: String,
        not_after: String,
    },
    #[error("failed to evaluate validity for certificate `{certificate}`")]
    CertificateValidity {
        certificate: String,
        #[source]
        source: openssl::error::ErrorStack,
    },
    #[error(
        "certificate `{certificate}` must contain at least one DNS or IP subject alternative name"
    )]
    MissingDnsSan { certificate: String },
    #[error("certificate `{certificate}` has invalid declared DNS/IP identities")]
    InvalidDeclaredDnsNames { certificate: String },
    #[error("failed to inspect DNS/IP subject alternative names for certificate `{certificate}`")]
    DnsSanInspection {
        certificate: String,
        #[source]
        source: Box<openssl::error::ErrorStack>,
    },
    #[error("certificate `{certificate}` contains a malformed DNS/IP subject alternative name")]
    InvalidDnsSanEncoding { certificate: String },
    #[error(
        "certificate `{certificate}` contains invalid DNS subject alternative name `{dns_name}`"
    )]
    InvalidDnsSan {
        certificate: String,
        dns_name: String,
    },
    #[error("certificate `{certificate}` does not contain every declared DNS/IP identity")]
    DnsSanMismatch { certificate: String },
    #[error("certificate `{certificate}` must supply at least one issuer after the leaf")]
    IncompleteCertificateChain { certificate: String },
    #[error(
        "certificate `{certificate}` chain entry {index} is not currently valid (not before {not_before})"
    )]
    ChainCertificateNotYetValid {
        certificate: String,
        index: usize,
        not_before: String,
    },
    #[error(
        "certificate `{certificate}` chain entry {index} is not currently valid (expired at {not_after})"
    )]
    ChainCertificateExpired {
        certificate: String,
        index: usize,
        not_after: String,
    },
    #[error(
        "certificate `{certificate}` chain entry {issuer_index} is not the issuer of entry {subject_index}: {reason}"
    )]
    InvalidChainIssuer {
        certificate: String,
        subject_index: usize,
        issuer_index: usize,
        reason: openssl::x509::X509VerifyResult,
    },
    #[error("certificate `{certificate}` chain entry {issuer_index} has an invalid public key")]
    ChainIssuerPublicKey {
        certificate: String,
        issuer_index: usize,
        #[source]
        source: Box<openssl::error::ErrorStack>,
    },
    #[error(
        "certificate `{certificate}` chain entry {subject_index} signature does not match issuer entry {issuer_index}"
    )]
    InvalidChainSignature {
        certificate: String,
        subject_index: usize,
        issuer_index: usize,
    },
    #[error("failed to validate certificate `{certificate}` chain entry {subject_index} signature")]
    ChainSignatureValidation {
        certificate: String,
        subject_index: usize,
        #[source]
        source: Box<openssl::error::ErrorStack>,
    },
    #[error("certificate `{certificate}` chain issuer entry {index} is not CA-capable")]
    NonCaChainIssuer { certificate: String, index: usize },
    #[error("failed to inspect certificate `{certificate}` chain issuer entry {index}")]
    ChainIssuerInspection {
        certificate: String,
        index: usize,
        #[source]
        source: Box<openssl::error::ErrorStack>,
    },
    #[error("failed to construct strict verification for certificate `{certificate}`")]
    ChainVerificationSetup {
        certificate: String,
        #[source]
        source: Box<openssl::error::ErrorStack>,
    },
    #[error(
        "strict SSL-server verification rejected certificate `{certificate}` at chain depth {depth}: {reason}"
    )]
    ChainVerification {
        certificate: String,
        depth: u32,
        reason: openssl::x509::X509VerifyResult,
    },
    #[error("strict verification produced an incomplete chain for certificate `{certificate}`")]
    IncompleteVerifiedChain { certificate: String },
    #[error("failed to derive public metadata for certificate `{certificate}`")]
    CertificateMetadata {
        certificate: String,
        #[source]
        source: openssl::error::ErrorStack,
    },
    #[error("OpenSSL acceptor preflight rejected certificate `{certificate}`")]
    AcceptorPreflight {
        certificate: String,
        #[source]
        source: openssl::error::ErrorStack,
    },
    #[error("duplicate certificate identity `{name}`")]
    DuplicateCertificate { name: String },
    #[error("TLS profile `{profile}` references unknown certificate `{certificate}`")]
    UnknownProfileCertificate {
        profile: String,
        certificate: String,
    },
    #[error("TLS profile `{profile}` references certificate `{certificate}` more than once")]
    DuplicateProfileCertificate {
        profile: String,
        certificate: String,
    },
    #[error(
        "TLS profile `{profile}` default certificate `{certificate}` is not in its certificate list"
    )]
    ProfileDefaultNotListed {
        profile: String,
        certificate: String,
    },
    #[error(
        "TLS profile `{profile}` assigns DNS name `{dns_name}` to both `{first_certificate}` and `{second_certificate}`"
    )]
    OverlappingProfileDnsName {
        profile: String,
        dns_name: String,
        first_certificate: String,
        second_certificate: String,
    },
    #[error("TLS profile `{profile}` has an unsupported ALPN policy")]
    InvalidProfileAlpn { profile: String },
    #[error("duplicate TLS profile identity `{name}`")]
    DuplicateTlsProfile { name: String },
    #[error("failed to create Pingora TLS settings for profile `{profile}`")]
    TlsSettings {
        profile: String,
        #[source]
        source: Box<pingora::Error>,
    },
    #[error("failed to initialize TLS-ALPN-01 certificate selection: {detail}")]
    TlsAlpnSelectionIndex { detail: String },
    #[error("failed to apply OpenSSL settings for TLS profile `{profile}`")]
    TlsProfileSettings {
        profile: String,
        #[source]
        source: openssl::error::ErrorStack,
    },
    #[error("failed to parse DH parameters `{path}` for TLS profile `{profile}`")]
    TlsDhParameters {
        profile: String,
        path: PathBuf,
        #[source]
        source: openssl::error::ErrorStack,
    },
    #[error("TLS profile `{profile}` has runtime TLS policy values outside OpenSSL limits")]
    InvalidTlsProfilePolicy { profile: String },
    #[error("TLS profile `{profile}` has invalid client-auth policy: {detail}")]
    InvalidTlsClientAuthPolicy {
        profile: String,
        detail: &'static str,
    },
    #[error("TLS profile `{profile}` client CA bundle contains too many certificates")]
    TooManyClientCaCertificates { profile: String },
    #[error("failed to parse client CA bundle `{path}` for TLS profile `{profile}`")]
    ClientCaParse {
        profile: String,
        path: PathBuf,
        #[source]
        source: openssl::error::ErrorStack,
    },
    #[error("client CA certificate {index} for TLS profile `{profile}` is not currently valid")]
    ClientCaCertificateInvalid {
        profile: String,
        index: usize,
        detail: &'static str,
    },
    #[error("client CA certificate {index} for TLS profile `{profile}` is not CA-capable")]
    NonCaClientCertificate { profile: String, index: usize },
    #[error("client CA certificate {index} for TLS profile `{profile}` could not be inspected")]
    ClientCaCertificateInspection {
        profile: String,
        index: usize,
        #[source]
        source: Box<openssl::error::ErrorStack>,
    },
    #[error("client CA certificate {index} for TLS profile `{profile}` is duplicated")]
    DuplicateClientCaCertificate { profile: String, index: usize },
    #[error(
        "client CA certificate {index} for TLS profile `{profile}` is not usable by rustls: {detail}"
    )]
    ClientCaRustlsCertificate {
        profile: String,
        index: usize,
        detail: String,
    },
    #[error("failed to construct the client CA trust store for TLS profile `{profile}`")]
    ClientCaStore {
        profile: String,
        #[source]
        source: Box<openssl::error::ErrorStack>,
    },
    #[error(
        "failed to construct the rustls client certificate verifier for TLS profile `{profile}`: {detail}"
    )]
    ClientCaRustlsVerifier { profile: String, detail: String },
    #[error("upstream pool `{pool}` has invalid TLS server name `{server_name}`")]
    InvalidUpstreamServerName { pool: String, server_name: String },
    #[error("upstream pool `{pool}` has an invalid HTTP version range")]
    InvalidHttpVersionRange { pool: String },
    #[error("custom CA bundle for upstream pool `{pool}` contains too many certificates")]
    TooManyCaCertificates { pool: String },
    #[error("custom CA bundle for upstream pool `{pool}` contains duplicate certificate {index}")]
    DuplicateCaCertificate { pool: String, index: usize },
    #[error(
        "custom CA certificate {index} for upstream pool `{pool}` is not currently valid (not before {not_before})"
    )]
    CaCertificateNotYetValid {
        pool: String,
        index: usize,
        not_before: String,
    },
    #[error(
        "custom CA certificate {index} for upstream pool `{pool}` is not currently valid (expired at {not_after})"
    )]
    CaCertificateExpired {
        pool: String,
        index: usize,
        not_after: String,
    },
    #[error("custom CA certificate {index} for upstream pool `{pool}` is not CA-capable")]
    NonCaCertificate { pool: String, index: usize },
    #[error("failed to inspect custom CA certificate {index} for upstream pool `{pool}`")]
    CaCertificateInspection {
        pool: String,
        index: usize,
        #[source]
        source: Box<openssl::error::ErrorStack>,
    },
    #[error("failed to construct custom CA trust store for upstream pool `{pool}`")]
    CaStore {
        pool: String,
        #[source]
        source: Box<openssl::error::ErrorStack>,
    },
    #[error("failed to parse custom CA bundle `{path}` for upstream pool `{pool}`")]
    CaParse {
        pool: String,
        path: PathBuf,
        #[source]
        source: openssl::error::ErrorStack,
    },
    #[error("failed to serialize custom CA policy for upstream pool `{pool}`")]
    CaPolicy {
        pool: String,
        #[source]
        source: openssl::error::ErrorStack,
    },
}
