#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountKeyAlgorithm {
    EcdsaP256,
    Rsa2048,
}

#[derive(Clone)]
pub struct AccountKey {
    private_key_pem: SecretBytes,
    algorithm: AccountKeyAlgorithm,
    jwk: BTreeMap<String, String>,
}

impl fmt::Debug for AccountKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AccountKey")
            .field("algorithm", &self.algorithm)
            .field("thumbprint", &self.thumbprint())
            .field("private_key", &"REDACTED")
            .finish_non_exhaustive()
    }
}

impl AccountKey {
    /// Generates an ACME account key with the selected maintained-library algorithm.
    ///
    /// # Errors
    ///
    /// Returns an error when OpenSSL cannot generate or serialize the key.
    pub fn generate(algorithm: AccountKeyAlgorithm) -> Result<Self, AcmeError> {
        let private_key = match algorithm {
            AccountKeyAlgorithm::EcdsaP256 => {
                let group =
                    EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).map_err(AcmeError::Key)?;
                PKey::from_ec_key(EcKey::generate(&group).map_err(AcmeError::Key)?)
                    .map_err(AcmeError::Key)?
            }
            AccountKeyAlgorithm::Rsa2048 => {
                PKey::from_rsa(Rsa::generate(2_048).map_err(AcmeError::Key)?)
                    .map_err(AcmeError::Key)?
            }
        };
        let private_key_pem = private_key
            .private_key_to_pem_pkcs8()
            .map_err(AcmeError::Key)?;
        let jwk = public_jwk(&private_key, algorithm)?;
        Ok(Self {
            private_key_pem: SecretBytes::new(private_key_pem),
            algorithm,
            jwk,
        })
    }

    /// Loads an unencrypted account key from owner-protected PEM bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when the key is unsupported, weak, malformed, or cannot produce its JWK.
    pub fn from_pem(private_key_pem: SecretBytes) -> Result<Self, AcmeError> {
        let key = PKey::private_key_from_pem(private_key_pem.as_bytes()).map_err(AcmeError::Key)?;
        let algorithm = match key.id() {
            Id::EC if key.bits() >= 256 => AccountKeyAlgorithm::EcdsaP256,
            Id::RSA | Id::RSA_PSS if key.bits() >= 2_048 => AccountKeyAlgorithm::Rsa2048,
            _ => return Err(AcmeError::MalformedResponse),
        };
        Ok(Self {
            jwk: public_jwk(&key, algorithm)?,
            private_key_pem,
            algorithm,
        })
    }

    #[must_use]
    pub const fn algorithm(&self) -> AccountKeyAlgorithm {
        self.algorithm
    }

    #[must_use]
    pub fn private_key_pem(&self) -> &SecretBytes {
        &self.private_key_pem
    }

    #[must_use]
    ///
    /// # Panics
    ///
    /// Panics only if the fixed string-valued JWK representation cannot be serialized.
    pub fn thumbprint(&self) -> String {
        let bytes = serde_json::to_vec(&self.jwk).expect("string-valued JWK serializes");
        URL_SAFE_NO_PAD.encode(Sha256::digest(&bytes))
    }

    #[must_use]
    pub fn key_authorization(&self, token: &str) -> String {
        format!("{token}.{}", self.thumbprint())
    }

    pub(super) fn protected_algorithm(&self) -> &'static str {
        match self.algorithm {
            AccountKeyAlgorithm::EcdsaP256 => "ES256",
            AccountKeyAlgorithm::Rsa2048 => "RS256",
        }
    }

    pub(super) fn jwk(&self) -> &BTreeMap<String, String> {
        &self.jwk
    }

    pub(super) fn sign(&self, input: &[u8]) -> Result<Vec<u8>, AcmeError> {
        let key =
            PKey::private_key_from_pem(self.private_key_pem.as_bytes()).map_err(AcmeError::Key)?;
        let digest = MessageDigest::sha256();
        let mut signer = Signer::new(digest, &key).map_err(AcmeError::Signature)?;
        signer.update(input).map_err(AcmeError::Signature)?;
        let signature = signer.sign_to_vec().map_err(AcmeError::Signature)?;
        if self.algorithm != AccountKeyAlgorithm::EcdsaP256 {
            return Ok(signature);
        }
        let signature = EcdsaSig::from_der(&signature).map_err(AcmeError::Signature)?;
        let mut raw = Vec::with_capacity(64);
        raw.extend(
            signature
                .r()
                .to_vec_padded(32)
                .map_err(AcmeError::Signature)?,
        );
        raw.extend(
            signature
                .s()
                .to_vec_padded(32)
                .map_err(AcmeError::Signature)?,
        );
        Ok(raw)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountRequest {
    pub contacts: Vec<String>,
    pub terms_agreed: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub url: String,
    pub status: String,
    pub contacts: Vec<String>,
    pub terms_agreed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertificateRequest {
    pub identifiers: Vec<String>,
}

fn public_jwk(
    key: &PKey<Private>,
    algorithm: AccountKeyAlgorithm,
) -> Result<BTreeMap<String, String>, AcmeError> {
    match algorithm {
        AccountKeyAlgorithm::EcdsaP256 => {
            let ec = key.ec_key().map_err(AcmeError::Key)?;
            let mut context = BigNumContext::new().map_err(AcmeError::Key)?;
            let public = ec
                .public_key()
                .to_bytes(ec.group(), PointConversionForm::UNCOMPRESSED, &mut context)
                .map_err(AcmeError::Key)?;
            if public.len() != 65 || public[0] != 4 {
                return Err(AcmeError::MalformedResponse);
            }
            Ok([
                ("crv".into(), "P-256".into()),
                ("kty".into(), "EC".into()),
                ("x".into(), URL_SAFE_NO_PAD.encode(&public[1..33])),
                ("y".into(), URL_SAFE_NO_PAD.encode(&public[33..65])),
            ]
            .into_iter()
            .collect())
        }
        AccountKeyAlgorithm::Rsa2048 => {
            let rsa = key.rsa().map_err(AcmeError::Key)?;
            Ok([
                ("e".into(), URL_SAFE_NO_PAD.encode(rsa.e().to_vec())),
                ("kty".into(), "RSA".into()),
                ("n".into(), URL_SAFE_NO_PAD.encode(rsa.n().to_vec())),
            ]
            .into_iter()
            .collect())
        }
    }
}

pub(super) fn validate_account_request(request: &AccountRequest) -> Result<(), AcmeError> {
    if !request.terms_agreed {
        return Err(AcmeError::TermsNotAgreed);
    }
    validate_account_contacts(&request.contacts)
}

pub(super) fn validate_account_contacts(contacts: &[String]) -> Result<(), AcmeError> {
    if contacts.len() > 8
        || contacts.iter().any(|contact| {
            contact.is_empty()
                || contact.len() > 320
                || !contact.is_ascii()
                || !contact.starts_with("mailto:")
                || contact[7..].contains(char::is_whitespace)
        })
    {
        return Err(AcmeError::InvalidAccountRequest);
    }
    Ok(())
}

pub(super) fn validate_account_status(status: &str) -> Result<(), AcmeError> {
    match status {
        "valid" => Ok(()),
        "deactivated" | "revoked" => Err(AcmeError::AccountNotUsable {
            status: status.into(),
        }),
        _ => Err(AcmeError::MalformedResponse),
    }
}

pub(super) fn parse_account(
    value: &Value,
    url: String,
    policy: &OriginPolicy,
    terms_requested: bool,
) -> Result<Account, AcmeError> {
    policy.permits(&url)?;
    let status = required_string(value, "status")?;
    validate_account_status(&status)?;
    let contacts = string_array(value, "contact")?;
    validate_account_contacts(&contacts)?;
    let terms_agreed = value
        .get("termsOfServiceAgreed")
        .and_then(Value::as_bool)
        .ok_or(AcmeError::MalformedResponse)?;
    if terms_requested && !terms_agreed {
        return Err(AcmeError::TermsNotAgreed);
    }
    Ok(Account {
        url,
        status,
        contacts,
        terms_agreed,
    })
}
