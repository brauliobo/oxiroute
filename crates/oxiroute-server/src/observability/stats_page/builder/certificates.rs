use super::super::model::{CertificateSource, CertificateStats, CertificateWatcherStats};
use crate::RuntimeSnapshot;

pub(crate) fn list(runtime: &RuntimeSnapshot) -> Vec<CertificateStats> {
    let certbot = runtime.certbot_certificates.iter().map(|certificate| {
        stats(
            CertificateSource::Certbot,
            &certificate.name,
            &certificate.expires_at,
            None,
            None,
            (&certificate.last_outcome, &certificate.last_error_code),
        )
    });
    let acme = runtime.acme_managed_certificates.iter().map(|certificate| {
        stats(
            CertificateSource::AcmeManaged,
            &certificate.name,
            &certificate.expires_at,
            certificate.not_after_unix_seconds,
            certificate.next_action_unix_seconds,
            (&certificate.last_outcome, &certificate.last_error_code),
        )
    });
    let direct = runtime.direct_file_certificates.iter().map(|certificate| {
        stats(
            CertificateSource::DirectFile,
            &certificate.name,
            &certificate.expires_at,
            None,
            None,
            (&certificate.last_outcome, &certificate.last_error_code),
        )
    });
    certbot.chain(acme).chain(direct).collect()
}

fn stats(
    source: CertificateSource,
    name: &str,
    expires_at: &str,
    not_after_unix_seconds: Option<u64>,
    next_action_unix_seconds: Option<u64>,
    (last_outcome, last_error_code): (&Option<String>, &Option<String>),
) -> CertificateStats {
    CertificateStats {
        name: name.to_owned(),
        source,
        expires_at: expires_at.to_owned(),
        not_after_unix_seconds: not_after_unix_seconds.or_else(|| parse_openssl_time(expires_at)),
        next_action_unix_seconds,
        last_outcome: last_outcome.clone(),
        last_error_code: last_error_code.clone(),
    }
}

pub(super) fn watchers(runtime: &RuntimeSnapshot) -> Vec<CertificateWatcherStats> {
    let certbot = runtime.certbot_watcher.as_ref().map(|watcher| {
        (
            CertificateSource::Certbot,
            watcher.health,
            watcher.backend_errors,
            watcher.reconciliation_failures,
        )
    });
    let direct = runtime.direct_file_watcher.as_ref().map(|watcher| {
        (
            CertificateSource::DirectFile,
            watcher.health,
            watcher.backend_errors,
            watcher.reconciliation_failures,
        )
    });
    certbot
        .into_iter()
        .chain(direct)
        .map(
            |(source, health, backend_errors, reconciliation_failures)| CertificateWatcherStats {
                source,
                health: health.into(),
                backend_errors: backend_errors.into(),
                reconciliation_failures: reconciliation_failures.into(),
            },
        )
        .collect()
}

/// Reads the `Mon DD HH:MM:SS YYYY GMT` form the TLS library prints for an ASN.1 time.
pub(crate) fn parse_openssl_time(printed: &str) -> Option<u64> {
    let mut parts = printed.split_whitespace();
    let name = parts.next()?;
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|month| *month == name)?
        + 1;
    let day = parts.next()?.parse::<u64>().ok()?;
    let mut clock = parts
        .next()?
        .split(':')
        .map(|part| part.parse::<u64>().ok());
    let (hour, minute, second) = (clock.next()??, clock.next()??, clock.next()??);
    let year = parts.next()?.parse::<u64>().ok()?;
    if parts.next()? != "GMT"
        || parts.next().is_some()
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let days = days_from_civil(year, month as u64, day)?;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days since 1970-01-01 of a proleptic Gregorian date on or after it.
fn days_from_civil(year: u64, month: u64, day: u64) -> Option<u64> {
    let year = if month <= 2 {
        year.checked_sub(1)?
    } else {
        year
    };
    let era = year / 400;
    let year_of_era = year % 400;
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    (era * 146_097 + day_of_era).checked_sub(719_468)
}
