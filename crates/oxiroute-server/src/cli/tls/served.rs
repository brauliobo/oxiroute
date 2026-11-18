use std::{
    fmt::Write as _,
    net::{IpAddr, TcpStream, ToSocketAddrs as _},
    path::Path,
};

use openssl::{
    hash::MessageDigest,
    ssl::{SslConnector, SslMethod, SslVerifyMode},
    stack::StackRef,
    x509::{X509, X509NameRef},
};
use serde_json::{Value, json};

use super::expiry::Expiry;
use crate::cli::{error::CliError, net::Timeouts};

#[derive(Clone, Debug)]
pub(crate) struct Target {
    host: String,
    port: u16,
}

impl Target {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        let (host, port) = value
            .rsplit_once(':')
            .ok_or("a target must look like HOST:PORT")?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let port = port.parse().map_err(|_| "the port must be a number")?;
        if host.is_empty() {
            return Err("a target must look like HOST:PORT".into());
        }
        Ok(Self {
            host: host.to_owned(),
            port,
        })
    }

    pub(super) fn host(&self) -> &str {
        &self.host
    }

    fn address(&self) -> String {
        if self.host.parse::<IpAddr>().is_ok_and(|ip| ip.is_ipv6()) {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.address())
    }
}

pub(crate) struct Served {
    pub(crate) target: Target,
    pub(crate) subject: String,
    pub(crate) issuer: String,
    pub(crate) dns_names: Vec<String>,
    pub(crate) not_before: Expiry,
    pub(crate) not_after: Expiry,
    pub(crate) serial: String,
    pub(crate) fingerprint_sha256: String,
    pub(crate) protocol: String,
    pub(crate) chain_length: usize,
    pub(crate) verification: Result<(), String>,
}

impl Served {
    pub(crate) fn details(&self, now: i64) -> Value {
        json!({
            "target": self.target.to_string(),
            "subject": self.subject,
            "issuer": self.issuer,
            "dnsNames": self.dns_names,
            "notBefore": self.not_before.utc(),
            "notAfter": self.not_after.utc(),
            "notAfterUnixSeconds": self.not_after.unix_seconds(),
            "remainingSeconds": self.not_after.remaining(now),
            "expires": self.not_after.relative(now),
            "serial": self.serial,
            "fingerprintSha256": self.fingerprint_sha256,
            "protocol": self.protocol,
            "chainLength": self.chain_length,
            "verified": self.verification.is_ok(),
            "verificationError": self.verification.as_ref().err(),
        })
    }
}

/// The certificate chain a TLS server presents, read without trusting it; `verification` says whether the chain and the
/// server name would have been trusted.
pub(crate) fn fetch(
    target: &Target,
    server_name: Option<&str>,
    ca_file: Option<&Path>,
    timeouts: Timeouts,
) -> Result<Served, CliError> {
    let local = |error: &dyn std::fmt::Display| CliError::Local(format!("{target}: {error}"));
    let mut builder = SslConnector::builder(SslMethod::tls_client()).map_err(|e| local(&e))?;
    if let Some(path) = ca_file {
        builder.set_ca_file(path).map_err(|e| local(&e))?;
    }
    builder.set_verify(SslVerifyMode::NONE);
    let address = target
        .address()
        .to_socket_addrs()
        .ok()
        .and_then(|mut addresses| addresses.next())
        .ok_or_else(|| CliError::Connect(format!("cannot resolve {target}")))?;
    let stream = TcpStream::connect_timeout(&address, timeouts.connect())
        .map_err(|error| CliError::Connect(format!("cannot connect to {target}: {error}")))?;
    for result in [
        stream.set_read_timeout(Some(timeouts.answer())),
        stream.set_write_timeout(Some(timeouts.answer())),
    ] {
        result.map_err(|e| local(&e))?;
    }
    let configuration = builder.build().configure().map_err(|e| local(&e))?;
    let tls = configuration
        .connect(server_name.unwrap_or_else(|| target.host()), stream)
        .map_err(|error| {
            CliError::Remote(format!("TLS handshake with {target} failed: {error}"))
        })?;
    let ssl = tls.ssl();
    let leaf = ssl
        .peer_certificate()
        .ok_or_else(|| CliError::Remote(format!("{target} presented no certificate")))?;
    let verified = ssl.verify_result();
    Ok(Served {
        target: target.clone(),
        subject: name(leaf.subject_name()),
        issuer: name(leaf.issuer_name()),
        dns_names: names(&leaf),
        not_before: Expiry::of_time(leaf.not_before()).map_err(|e| local(&e))?,
        not_after: Expiry::of_time(leaf.not_after()).map_err(|e| local(&e))?,
        serial: leaf
            .serial_number()
            .to_bn()
            .and_then(|serial| serial.to_hex_str().map(|hex| hex.to_string()))
            .map_err(|e| local(&e))?,
        fingerprint_sha256: leaf
            .digest(MessageDigest::sha256())
            .map(|digest| hex(&digest))
            .map_err(|e| local(&e))?,
        protocol: ssl.version_str().to_owned(),
        chain_length: ssl.peer_cert_chain().map_or(1, StackRef::len),
        verification: if verified == openssl::x509::X509VerifyResult::OK {
            Ok(())
        } else {
            Err(verified.error_string().to_owned())
        },
    })
}

fn name(name: &X509NameRef) -> String {
    name.entries()
        .filter_map(|entry| {
            let value = entry.data().to_string().ok()?;
            Some(format!(
                "{}={value}",
                entry.object().nid().short_name().ok()?
            ))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn names(certificate: &X509) -> Vec<String> {
    certificate
        .subject_alt_names()
        .into_iter()
        .flatten()
        .filter_map(|name| name.dnsname().map(str::to_owned))
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}
