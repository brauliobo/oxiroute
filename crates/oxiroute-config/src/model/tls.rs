#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Certificate {
    pub name: String,
    pub dns_names: Vec<String>,
    pub source: CertificateSource,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CertificateSource {
    Files {
        certificate_chain_path: PathBuf,
        private_key_path: PathBuf,
    },
    Certbot {
        live_directory_path: PathBuf,
        archive_directory_path: PathBuf,
    },
    AcmeManaged {
        directory_url: String,
        state_root: PathBuf,
        #[serde(default)]
        contacts: Vec<String>,
        terms_agreed: bool,
        #[serde(default)]
        challenge: AcmeChallengeType,
        #[serde(default)]
        key_type: AcmeKeyType,
        allowed_dns_suffixes: Vec<String>,
        #[serde(default = "default_acme_retained_revisions")]
        retained_revisions: u32,
        #[serde(default = "default_acme_retention_days")]
        retention_days: u32,
        #[serde(default)]
        dns01: Option<AcmeDns01Config>,
    },
    SelfSignedDevelopment {
        #[serde(default = "default_self_signed_validity_days")]
        validity_days: u32,
        #[serde(default)]
        key_type: SelfSignedKeyType,
    },
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AcmeChallengeType {
    #[default]
    Http01,
    Dns01,
    TlsAlpn01,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AcmeDns01Config {
    pub provider: String,
    pub credential_file: PathBuf,
    #[serde(default = "default_acme_dns01_timeout_seconds")]
    pub timeout_seconds: u64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AcmeKeyType {
    #[default]
    EcdsaP256,
    #[serde(rename = "rsa_2048")]
    Rsa2048,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SelfSignedKeyType {
    #[default]
    EcdsaP256,
    #[serde(rename = "rsa_2048")]
    Rsa2048,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TlsProfile {
    pub name: String,
    pub certificates: Vec<String>,
    pub default_certificate: String,
    #[serde(default)]
    pub min_version: TlsVersion,
    #[serde(default = "default_alpn")]
    pub alpn: Vec<AlpnProtocol>,
    #[serde(default)]
    pub policy: TlsPolicy,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TlsPolicy {
    #[serde(default)]
    pub cipher_list: Option<String>,
    #[serde(default)]
    pub dh_parameters_path: Option<PathBuf>,
    #[serde(default)]
    pub client_auth: TlsClientAuthPolicy,
    #[serde(default)]
    pub session_cache: Option<TlsSessionCache>,
    #[serde(default)]
    pub session_timeout_seconds: Option<u64>,
    #[serde(default)]
    pub session_tickets: bool,
    #[serde(default = "default_true")]
    pub prefer_server_ciphers: bool,
    /// Key-exchange groups in server preference order; absent keeps the backend default list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_exchange_groups: Option<Vec<TlsKeyExchangeGroup>>,
}

impl Default for TlsPolicy {
    fn default() -> Self {
        Self {
            cipher_list: None,
            dh_parameters_path: None,
            client_auth: TlsClientAuthPolicy::default(),
            session_cache: None,
            session_timeout_seconds: None,
            session_tickets: false,
            prefer_server_ciphers: true,
            key_exchange_groups: None,
        }
    }
}

/// A key-exchange group every TLS backend can name; `X25519Mlkem768` is the post-quantum hybrid (TLS 1.3 only).
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TlsKeyExchangeGroup {
    X25519,
    Secp256r1,
    Secp384r1,
    X25519Mlkem768,
}

impl TlsKeyExchangeGroup {
    #[must_use]
    pub const fn is_hybrid(self) -> bool {
        matches!(self, Self::X25519Mlkem768)
    }

    /// The group as TLS 1.3 clients and the key-exchange registry name it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::X25519 => "x25519",
            Self::Secp256r1 => "secp256r1",
            Self::Secp384r1 => "secp384r1",
            Self::X25519Mlkem768 => "x25519mlkem768",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TlsClientAuthMode {
    #[default]
    Disabled,
    Optional,
    Required,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TlsClientAuthPolicy {
    #[serde(default)]
    pub mode: TlsClientAuthMode,
    #[serde(default)]
    pub ca_certificate_path: Option<PathBuf>,
    #[serde(default)]
    pub allowed_dns_names: Vec<String>,
}

impl Default for TlsClientAuthPolicy {
    fn default() -> Self {
        Self {
            mode: TlsClientAuthMode::Disabled,
            ca_certificate_path: None,
            allowed_dns_names: Vec::new(),
        }
    }
}

impl fmt::Debug for TlsClientAuthPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TlsClientAuthPolicy")
            .field("mode", &self.mode)
            .field(
                "ca_certificate_configured",
                &self.ca_certificate_path.is_some(),
            )
            .field("allowed_dns_name_count", &self.allowed_dns_names.len())
            .finish()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TlsSessionCache {
    pub name: String,
    pub size_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum TlsVersion {
    #[default]
    #[serde(rename = "1.2")]
    Tls12,
    #[serde(rename = "1.3")]
    Tls13,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum AlpnProtocol {
    #[serde(rename = "h3")]
    H3,
    #[serde(rename = "h2")]
    H2,
    #[serde(rename = "http/1.1")]
    Http11,
}
