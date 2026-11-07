#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Order {
    pub url: String,
    pub status: String,
    pub identifiers: Vec<String>,
    pub authorizations: Vec<String>,
    pub finalize: Option<String>,
    pub certificate: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChallengeType {
    Http01,
    Dns01,
    TlsAlpn01,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthorizationStatus {
    Pending,
    Processing,
    Valid,
    Invalid,
    Deactivated,
    Expired,
    Revoked,
}

impl AuthorizationStatus {
    fn parse(value: &str) -> Result<Self, AcmeError> {
        match value {
            "pending" => Ok(Self::Pending),
            "processing" => Ok(Self::Processing),
            "valid" => Ok(Self::Valid),
            "invalid" => Ok(Self::Invalid),
            "deactivated" => Ok(Self::Deactivated),
            "expired" => Ok(Self::Expired),
            "revoked" => Ok(Self::Revoked),
            _ => Err(AcmeError::MalformedResponse),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Authorization {
    pub url: String,
    pub identifier: String,
    pub status: AuthorizationStatus,
    pub challenge: Option<Http01Challenge>,
    pub dns01_challenge: Option<Dns01Challenge>,
    pub tls_alpn01_challenge: Option<TlsAlpn01Challenge>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Http01Challenge {
    pub authorization_url: String,
    pub challenge_url: String,
    pub token: String,
    pub key_authorization: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TlsAlpn01Challenge {
    pub authorization_url: String,
    pub challenge_url: String,
    pub token: String,
    pub key_authorization: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeafKeyAlgorithm {
    EcdsaP256,
    Rsa2048,
}

#[derive(Clone)]
pub struct LeafCsr {
    pub private_key_pem: SecretBytes,
    pub csr_der: Vec<u8>,
    pub csr_pem: Vec<u8>,
}

impl fmt::Debug for LeafCsr {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LeafCsr")
            .field("private_key", &"REDACTED")
            .field("csr_der_bytes", &self.csr_der.len())
            .field("csr_pem_bytes", &self.csr_pem.len())
            .finish_non_exhaustive()
    }
}

/// Generates a new leaf key and an exact-SAN PKCS#10 request.
///
/// # Errors
///
/// Returns an error when identifiers are unsupported or OpenSSL rejects key, SAN, or CSR creation.
pub fn generate_leaf_csr(
    identifiers: &[String],
    algorithm: LeafKeyAlgorithm,
) -> Result<LeafCsr, AcmeError> {
    let identifiers = normalize_identifiers(identifiers)?;
    let key = match algorithm {
        LeafKeyAlgorithm::EcdsaP256 => {
            let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).map_err(AcmeError::Csr)?;
            PKey::from_ec_key(EcKey::generate(&group).map_err(AcmeError::Csr)?)
                .map_err(AcmeError::Csr)?
        }
        LeafKeyAlgorithm::Rsa2048 => {
            PKey::from_rsa(Rsa::generate(2_048).map_err(AcmeError::Csr)?).map_err(AcmeError::Csr)?
        }
    };
    let private_key_pem = SecretBytes::new(key.private_key_to_pem_pkcs8().map_err(AcmeError::Csr)?);
    let mut subject = X509NameBuilder::new().map_err(AcmeError::Csr)?;
    subject
        .append_entry_by_text("commonName", &identifiers[0])
        .map_err(AcmeError::Csr)?;
    let subject = subject.build();
    let mut builder = X509ReqBuilder::new().map_err(AcmeError::Csr)?;
    builder.set_version(0).map_err(AcmeError::Csr)?;
    builder.set_subject_name(&subject).map_err(AcmeError::Csr)?;
    builder.set_pubkey(&key).map_err(AcmeError::Csr)?;
    let mut san = SubjectAlternativeName::new();
    for identifier in &identifiers {
        san.dns(identifier);
    }
    let context = builder.x509v3_context(None);
    let extension = san.build(&context).map_err(AcmeError::Csr)?;
    let mut extensions = Stack::new().map_err(AcmeError::Csr)?;
    extensions.push(extension).map_err(AcmeError::Csr)?;
    builder
        .add_extensions(&extensions)
        .map_err(AcmeError::Csr)?;
    builder
        .sign(&key, MessageDigest::sha256())
        .map_err(AcmeError::Csr)?;
    let csr: X509Req = builder.build();
    Ok(LeafCsr {
        private_key_pem,
        csr_der: csr.to_der().map_err(AcmeError::Csr)?,
        csr_pem: csr.to_pem().map_err(AcmeError::Csr)?,
    })
}

fn validate_order_status(status: &str) -> Result<(), AcmeError> {
    matches!(
        status,
        "pending" | "ready" | "processing" | "valid" | "invalid"
    )
    .then_some(())
    .ok_or_else(|| AcmeError::InvalidOrderState {
        status: status.into(),
    })
}

pub(super) fn normalize_identifiers(identifiers: &[String]) -> Result<Vec<String>, AcmeError> {
    if identifiers.is_empty() || identifiers.len() > MAX_IDENTIFIERS {
        return Err(AcmeError::UnsupportedIdentifier);
    }
    let mut normalized = BTreeSet::new();
    for identifier in identifiers {
        let identifier = identifier.trim().to_ascii_lowercase();
        if identifier.is_empty() || identifier.len() > 253 {
            return Err(AcmeError::UnsupportedIdentifier);
        }
        if identifier.parse::<IpAddr>().is_ok() {
            return Err(AcmeError::IpIdentifierUnsupported);
        }
        let dns_name = identifier.strip_prefix("*.").unwrap_or(&identifier);
        if identifier.contains('*') && !identifier.starts_with("*.") {
            return Err(AcmeError::UnsupportedIdentifier);
        }
        if !valid_dns_name(dns_name) || dns_name.parse::<IpAddr>().is_ok() {
            return Err(AcmeError::UnsupportedIdentifier);
        }
        normalized.insert(identifier);
    }
    if normalized.len() != identifiers.len() {
        return Err(AcmeError::UnsupportedIdentifier);
    }
    Ok(normalized.into_iter().collect())
}

fn valid_dns_name(value: &str) -> bool {
    value.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    })
}

pub(super) fn parse_order(
    response: &HttpResponse,
    policy: &OriginPolicy,
) -> Result<Order, AcmeError> {
    let value = bounded_json(&response.body, MAX_ACME_BODY_BYTES)?;
    let url = response
        .header("location")
        .map_or_else(|| response.url.clone(), str::to_owned);
    policy.permits(&url)?;
    let identifiers = value
        .get("identifiers")
        .and_then(Value::as_array)
        .ok_or(AcmeError::MissingField)?
        .iter()
        .map(|identifier| {
            match identifier.get("type").and_then(Value::as_str) {
                Some("dns") => {}
                Some("ip") => return Err(AcmeError::IpIdentifierUnsupported),
                _ => return Err(AcmeError::UnsupportedIdentifier),
            }
            identifier
                .get("value")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or(AcmeError::MissingField)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let identifiers = normalize_identifiers(&identifiers)?;
    let authorizations = value
        .get("authorizations")
        .and_then(Value::as_array)
        .ok_or(AcmeError::MissingField)?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or(AcmeError::MalformedResponse)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if authorizations.len() > MAX_IDENTIFIERS {
        return Err(AcmeError::MalformedResponse);
    }
    let finalize = optional_string(&value, "finalize")?;
    let certificate = optional_string(&value, "certificate")?;
    for endpoint in authorizations
        .iter()
        .chain(finalize.iter())
        .chain(certificate.iter())
    {
        policy.permits(endpoint)?;
    }
    let status = required_string(&value, "status")?;
    validate_order_status(&status)?;
    Ok(Order {
        url,
        status,
        identifiers,
        authorizations,
        finalize,
        certificate,
    })
}

#[allow(clippy::too_many_lines)]
pub(super) fn parse_authorization(
    response: &HttpResponse,
    url: &str,
    key: &AccountKey,
    policy: &OriginPolicy,
    challenge_type: ChallengeType,
) -> Result<Authorization, AcmeError> {
    let value = bounded_json(&response.body, MAX_ACME_BODY_BYTES)?;
    let identifier = value
        .get("identifier")
        .and_then(|value| value.get("value"))
        .and_then(Value::as_str)
        .ok_or(AcmeError::MissingField)?
        .to_ascii_lowercase();
    normalize_identifiers(std::slice::from_ref(&identifier))?;
    let status = AuthorizationStatus::parse(&required_string(&value, "status")?)?;
    let challenges = value
        .get("challenges")
        .and_then(Value::as_array)
        .ok_or(AcmeError::MissingField)?;
    let http01_challenge = challenges
        .iter()
        .find(|challenge| challenge.get("type").and_then(Value::as_str) == Some("http-01"));
    let challenge = http01_challenge
        .map(|challenge| {
            let ChallengeValue {
                challenge_url,
                token,
            } = parse_challenge_value(challenge, policy)?;
            Ok(Http01Challenge {
                authorization_url: url.into(),
                challenge_url,
                token: token.clone(),
                key_authorization: key.key_authorization(&token),
            })
        })
        .transpose()?;
    let dns01_challenge = challenges
        .iter()
        .find(|challenge| challenge.get("type").and_then(Value::as_str) == Some("dns-01"))
        .map(|challenge| {
            let ChallengeValue {
                challenge_url,
                token,
            } = parse_challenge_value(challenge, policy)?;
            let dns_name = identifier.strip_prefix("*.").unwrap_or(&identifier);
            let record_name = format!("_acme-challenge.{dns_name}");
            let key_authorization = key.key_authorization(&token);
            let record_value = URL_SAFE_NO_PAD.encode(Sha256::digest(key_authorization.as_bytes()));
            Dns01Challenge::new(&identifier, challenge_url, record_name, record_value)
                .map_err(|_| AcmeError::MalformedResponse)
        })
        .transpose()?;
    let tls_alpn01_challenge = challenges
        .iter()
        .find(|challenge| challenge.get("type").and_then(Value::as_str) == Some("tls-alpn-01"))
        .map(|challenge| {
            let ChallengeValue {
                challenge_url,
                token,
            } = parse_challenge_value(challenge, policy)?;
            Ok(TlsAlpn01Challenge {
                authorization_url: url.into(),
                challenge_url,
                token: token.clone(),
                key_authorization: key.key_authorization(&token),
            })
        })
        .transpose()?;
    let selected = match challenge_type {
        ChallengeType::Http01 => challenge.is_some(),
        ChallengeType::Dns01 => dns01_challenge.is_some(),
        ChallengeType::TlsAlpn01 => tls_alpn01_challenge.is_some(),
    };
    if !selected
        && matches!(
            status,
            AuthorizationStatus::Pending | AuthorizationStatus::Processing
        )
    {
        return Err(AcmeError::UnsupportedChallenge);
    }
    Ok(Authorization {
        url: url.into(),
        identifier,
        status,
        challenge,
        dns01_challenge,
        tls_alpn01_challenge,
    })
}

struct ChallengeValue {
    challenge_url: String,
    token: String,
}

fn parse_challenge_value(
    challenge: &Value,
    policy: &OriginPolicy,
) -> Result<ChallengeValue, AcmeError> {
    let challenge_url = challenge
        .get("url")
        .and_then(Value::as_str)
        .ok_or(AcmeError::MissingField)?
        .to_owned();
    policy.permits(&challenge_url)?;
    let token = challenge
        .get("token")
        .and_then(Value::as_str)
        .ok_or(AcmeError::MissingField)?
        .to_owned();
    validate_token(&token)?;
    Ok(ChallengeValue {
        challenge_url,
        token,
    })
}

fn validate_token(token: &str) -> Result<(), AcmeError> {
    if token.is_empty()
        || token.len() > 256
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(AcmeError::MalformedResponse);
    }
    Ok(())
}
