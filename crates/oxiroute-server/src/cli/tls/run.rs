use std::time::Duration;

use serde_json::{Value, json};

use super::{
    args::{AcmeCommand, TlsOperation},
    expiry::Expiry,
    served,
};
use crate::cli::{
    context::{Context, Execute},
    error::CliError,
    units::now_unix_seconds,
    view::View,
};

impl Execute for TlsOperation {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        match self {
            Self::List(args) => list(context, args.expiring_within),
            Self::Show { certificate } => context.show_named(
                "/api/v1/tls",
                "certificates",
                certificate,
                View::TlsCertificate,
            ),
            Self::Inspect {
                target,
                servername,
                ca_file,
            } => {
                let served = served::fetch(
                    target,
                    servername.as_deref(),
                    ca_file.as_deref(),
                    context.timeouts()?,
                )?;
                context.print(&served.details(now_unix_seconds()), View::TlsServed)
            }
            Self::Reconcile { certificate } => post(context, "reconcile", certificate.as_deref()),
            Self::Renew { certificate } => post(context, "renew", certificate.as_deref()),
            Self::Revoke {
                certificate,
                reason,
                ..
            } => context.mutate(
                "POST",
                "/api/v1/tls/revoke",
                &json!({ "certificate": certificate, "reason": reason }),
            ),
            Self::Delete { certificate, .. } => post(context, "delete", Some(certificate)),
            Self::Acme { command } => command.run(context),
        }
    }
}

impl Execute for AcmeCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        let (path, certificate) = match self {
            Self::Rollover { certificate } => ("account/rollover", certificate),
            Self::Pause { certificate } => ("jobs/pause", certificate),
            Self::Resume { certificate } => ("jobs/resume", certificate),
            Self::Cancel { certificate } => ("jobs/cancel", certificate),
        };
        post(context, path, certificate.as_deref())
    }
}

fn post(context: &Context<'_>, action: &str, certificate: Option<&str>) -> Result<(), CliError> {
    context.mutate(
        "POST",
        &format!("/api/v1/tls/{action}"),
        &json!({ "certificate": certificate }),
    )
}

fn list(context: &Context<'_>, within: Option<Duration>) -> Result<(), CliError> {
    let Some(within) = within else {
        return context.show("/api/v1/tls", View::Tls);
    };
    let mut value = context.client()?.get("/api/v1/tls", true)?.json()?;
    let limit =
        now_unix_seconds().saturating_add(i64::try_from(within.as_secs()).unwrap_or(i64::MAX));
    if let Some(certificates) = value.get_mut("certificates").and_then(Value::as_array_mut) {
        certificates.retain(|certificate| {
            Expiry::of_status(&certificate["status"])
                .is_some_and(|expiry| expiry.unix_seconds() <= limit)
        });
    }
    context.print(&value, View::Tls)
}
