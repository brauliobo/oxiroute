use std::time::Duration;

use clap::{Args, Subcommand};

use super::{check::CheckArgs, served::Target};
use crate::cli::parse::{nonempty_target, parse_duration};

#[derive(Subcommand)]
pub(crate) enum TlsCommand {
    /// Judge certificate expiry as a monitoring plugin: 0 OK, 1 WARNING, 2 CRITICAL, 3 UNKNOWN.
    ///
    /// Without HOST:PORT it judges the certificates of the daemon, read with the management token.
    /// With HOST:PORT it handshakes with that TLS server and judges the certificate it presents,
    /// without a token. Like `oxiroute check` it does not use the exit categories 2 to 9.
    Check(CheckArgs),
    #[command(flatten)]
    Operation(TlsOperation),
}

#[derive(Subcommand)]
pub(crate) enum TlsOperation {
    /// List the certificates with their source, expiry in UTC, time left and last renewal outcome.
    List(ListArgs),
    /// Show one certificate with its expiry and renewal state.
    Show {
        /// Name of the certificate, as printed by `tls list`.
        #[arg(value_parser = nonempty_target)]
        certificate: String,
    },
    /// Show the certificate a TLS server presents, with its expiry in UTC and time left.
    Inspect {
        /// HOST:PORT of the TLS server.
        #[arg(value_name = "HOST:PORT", value_parser = Target::parse)]
        target: Target,
        /// Server name to ask for with SNI; default: the host.
        #[arg(long, value_name = "NAME")]
        servername: Option<String>,
        /// Extra trusted root certificates (PEM) used to report whether the chain is trusted.
        #[arg(long, value_name = "FILE")]
        ca_file: Option<std::path::PathBuf>,
    },
    /// Reload certificate files from disk now instead of waiting for the watcher.
    Reconcile {
        /// Only this certificate; default: every certificate.
        #[arg(long, value_parser = nonempty_target)]
        certificate: Option<String>,
    },
    /// Renew an ACME-managed certificate now.
    Renew {
        /// Only this certificate; default: every managed certificate.
        #[arg(long, value_parser = nonempty_target)]
        certificate: Option<String>,
    },
    /// Revoke an ACME-managed certificate at its issuer.
    Revoke {
        /// Name of the certificate.
        #[arg(value_parser = nonempty_target)]
        certificate: String,
        /// RFC 5280 revocation reason code, 0 to 10.
        #[arg(long, value_parser = clap::value_parser!(u8).range(..=10))]
        reason: Option<u8>,
        /// Confirm the revocation, which cannot be undone.
        #[arg(long, required = true)]
        yes: bool,
    },
    /// Delete the stored state of an ACME-managed certificate that no TLS profile uses.
    Delete {
        /// Name of the certificate.
        #[arg(value_parser = nonempty_target)]
        certificate: String,
        /// Confirm the deletion, which cannot be undone.
        #[arg(long, required = true)]
        yes: bool,
    },
    /// Control the ACME account and the running certificate jobs.
    Acme {
        #[command(subcommand)]
        command: AcmeCommand,
    },
}

#[derive(Args)]
pub(crate) struct ListArgs {
    /// Only certificates that expire within this long or have expired, for example 30d.
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    pub(super) expiring_within: Option<Duration>,
}

#[derive(Subcommand)]
pub(crate) enum AcmeCommand {
    /// Replace the ACME account key and register the certificates with the new one.
    Rollover {
        /// Only this certificate; default: every managed certificate.
        #[arg(long, value_parser = nonempty_target)]
        certificate: Option<String>,
    },
    /// Pause the renewal job of a certificate.
    Pause {
        /// Only this certificate; default: every managed certificate.
        #[arg(long, value_parser = nonempty_target)]
        certificate: Option<String>,
    },
    /// Resume the paused renewal job of a certificate.
    Resume {
        /// Only this certificate; default: every managed certificate.
        #[arg(long, value_parser = nonempty_target)]
        certificate: Option<String>,
    },
    /// Cancel the running renewal job of a certificate.
    Cancel {
        /// Only this certificate; default: every managed certificate.
        #[arg(long, value_parser = nonempty_target)]
        certificate: Option<String>,
    },
}
