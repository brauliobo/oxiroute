//! One OpenSSL client context for every outbound connection that trusts the system store.
//!
//! Building an `SslConnector` loads and parses the whole system CA bundle (about 120 M instructions and
//! 1.2 MB of heap). Short-lived callers (RTMPS connects, VOD origin fetches, RTMP callbacks, ACME requests)
//! used to pay that for every connection; they share the context built on first use instead. The context
//! is immutable: certificate and hostname verification are configured per connection by `connect`.
use std::sync::OnceLock;

use openssl::error::ErrorStack;
use openssl::ssl::{SslConnector, SslMethod};

static SYSTEM_TRUST: OnceLock<SslConnector> = OnceLock::new();

/// Returns the shared connector, building it (one CA bundle parse) on the first call.
///
/// # Errors
///
/// Returns the OpenSSL error stack when the context cannot be created. A failed build is retried by the
/// next call.
pub fn system_trust_connector() -> Result<&'static SslConnector, ErrorStack> {
    if let Some(connector) = SYSTEM_TRUST.get() {
        return Ok(connector);
    }
    let connector = SslConnector::builder(SslMethod::tls_client())?.build();
    Ok(SYSTEM_TRUST.get_or_init(|| connector))
}
