#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OriginPolicy {
    allowed_origins: BTreeSet<String>,
}

impl OriginPolicy {
    /// Creates a policy for a public HTTPS directory origin.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed, non-HTTPS, or local origins.
    pub fn strict(directory_url: &str) -> Result<Self, AcmeError> {
        let origin = origin_of(directory_url)?;
        if is_private_origin(&origin) {
            return Err(AcmeError::PrivateOriginRequiresAllowlist);
        }
        Ok(Self {
            allowed_origins: [origin].into_iter().collect(),
        })
    }

    /// Creates an explicitly allowlisted policy for deterministic local or private tests.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed or non-HTTPS URLs.
    pub fn development_allowlist(directory_url: &str) -> Result<Self, AcmeError> {
        Ok(Self {
            allowed_origins: [origin_of(directory_url)?].into_iter().collect(),
        })
    }

    /// Adds one advertised HTTPS origin to the explicit outbound allowlist.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed or non-HTTPS URLs.
    pub fn allow_origin(mut self, url: &str) -> Result<Self, AcmeError> {
        self.allowed_origins.insert(origin_of(url)?);
        Ok(self)
    }

    pub(super) fn permits(&self, url: &str) -> Result<(), AcmeError> {
        if url.len() > MAX_ACME_URL_BYTES || !self.allowed_origins.contains(&origin_of(url)?) {
            return Err(AcmeError::EndpointOutsidePolicy);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryDocument {
    pub new_nonce: String,
    pub new_account: String,
    pub new_order: String,
    pub revoke_certificate: Option<String>,
    pub key_change: Option<String>,
    pub renewal_info: Option<String>,
    pub terms_of_service: Option<String>,
    pub external_account_required: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Directory {
    pub url: String,
    pub document: DirectoryDocument,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenewalInformation {
    pub suggested_window_start_unix_seconds: u64,
    pub suggested_window_end_unix_seconds: u64,
}

impl Directory {
    /// Fetches and validates an ACME v2 directory through an injected transport.
    ///
    /// # Errors
    ///
    /// Returns an error for an untrusted origin, redirect, oversized body, malformed document, or
    /// transport failure.
    pub fn fetch<T: AcmeTransport>(
        transport: &T,
        url: &str,
        policy: &OriginPolicy,
    ) -> Result<Self, AcmeError> {
        Self::fetch_with_operation(transport, url, policy, &AcmeOperation::default())
    }

    /// Fetches a directory under a caller-owned cancellation operation.
    ///
    /// # Errors
    ///
    /// Returns the same validation and transport errors as [`Self::fetch`], plus
    /// [`AcmeError::Cancelled`] when cancellation or the operation deadline wins.
    pub fn fetch_with_operation<T: AcmeTransport>(
        transport: &T,
        url: &str,
        policy: &OriginPolicy,
        operation: &AcmeOperation,
    ) -> Result<Self, AcmeError> {
        policy.permits(url)?;
        let request = HttpRequest::new("GET", url, Vec::new());
        let response = transport.request_with_operation(request, operation)?;
        validate_response_url(url, &response, policy)?;
        if response.status / 100 == 3 {
            return Err(AcmeError::UntrustedRedirect);
        }
        if response.status != 200 {
            return Err(problem_or_status(&response));
        }
        let value = bounded_json(&response.body, MAX_ACME_BODY_BYTES)?;
        let document = DirectoryDocument {
            new_nonce: required_string(&value, "newNonce")?,
            new_account: required_string(&value, "newAccount")?,
            new_order: required_string(&value, "newOrder")?,
            revoke_certificate: optional_string(&value, "revokeCert")?,
            key_change: optional_string(&value, "keyChange")?,
            renewal_info: optional_string(&value, "renewalInfo")?,
            terms_of_service: value
                .get("meta")
                .and_then(|meta| meta.get("termsOfService"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            external_account_required: value
                .get("meta")
                .and_then(|meta| meta.get("externalAccountRequired"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
        };
        for endpoint in [
            Some(document.new_nonce.as_str()),
            Some(document.new_account.as_str()),
            Some(document.new_order.as_str()),
            document.revoke_certificate.as_deref(),
            document.key_change.as_deref(),
            document.renewal_info.as_deref(),
            document.terms_of_service.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            policy.permits(endpoint)?;
        }
        Ok(Self {
            url: url.into(),
            document,
        })
    }
}
