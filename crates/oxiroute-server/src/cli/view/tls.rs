use serde_json::Value;

use super::read::{flag, items, joined, number, text};
use crate::cli::{
    render::{Block, Document, Field, Record},
    tls::{Expiry, now_unix_seconds},
};

const DETAIL_TEXT: [(&str, &str); 10] = [
    ("active_revision", "activeRevision"),
    ("disk_revision", "diskRevision"),
    ("challenge", "challenge"),
    ("directory", "directoryUrl"),
    ("key_type", "keyType"),
    ("dns_provider", "dnsProvider"),
    ("job", "jobStatus"),
    ("job_id", "jobId"),
    ("renewal_information", "renewalInformationStatus"),
    ("dns_cleanup", "dnsCleanupStatus"),
];

pub(super) fn list(value: &Value) -> Document {
    list_at(value, now_unix_seconds())
}

pub(super) fn detail(value: &Value) -> Document {
    detail_at(value, now_unix_seconds())
}

fn list_at(value: &Value, now: i64) -> Document {
    let mut blocks = vec![Block::Rows {
        name: "certificates".into(),
        rows: items(value, "certificates")
            .iter()
            .map(|certificate| row(certificate, now))
            .collect(),
    }];
    if let Some(watcher) = value.get("watcher").filter(|watcher| watcher.is_object()) {
        blocks.push(Block::Record {
            name: "watcher".into(),
            fields: watcher_fields(watcher),
        });
    }
    Document::new(blocks)
}

fn detail_at(value: &Value, now: i64) -> Document {
    Document::new(
        items(value, "certificates")
            .iter()
            .map(|certificate| Block::Record {
                name: "certificate".into(),
                fields: record(certificate, now),
            })
            .collect(),
    )
}

fn expiry_fields(status: &Value, now: i64) -> [Field; 2] {
    let expiry = Expiry::of_status(status);
    [
        Field::text("expires", expiry.map(Expiry::utc).as_deref()),
        Field::text("left", expiry.map(|expiry| expiry.relative(now)).as_deref()),
    ]
}

fn row(certificate: &Value, now: i64) -> Record {
    let status = certificate.get("status").unwrap_or(&Value::Null);
    let mut fields = vec![
        Field::text("name", text(certificate, "name")),
        Field::text("source", text(certificate, "source")),
        Field::text("dns_names", joined(certificate, "dnsNames").as_deref()),
    ];
    fields.extend(expiry_fields(status, now));
    fields.extend([
        Field::text("outcome", text(status, "lastOutcome")),
        Field::text("error", text(status, "lastErrorCode")),
    ]);
    fields
}

fn record(certificate: &Value, now: i64) -> Record {
    let status = certificate.get("status").unwrap_or(&Value::Null);
    let mut fields = vec![
        Field::text("name", text(certificate, "name")),
        Field::text("source", text(certificate, "source")),
        Field::text("dns_names", joined(certificate, "dnsNames").as_deref()),
        Field::flag("development_only", flag(certificate, "developmentOnly")),
    ];
    fields.extend(expiry_fields(status, now));
    fields.extend([
        Field::timestamp(
            "next_action",
            number(status, "nextActionUnixSeconds").map(|seconds| seconds * 1_000),
        ),
        Field::timestamp(
            "last_success",
            number(status, "lastSuccessUnixSeconds").map(|seconds| seconds * 1_000),
        ),
        Field::flag("paused", flag(status, "paused")),
        Field::number("retry_attempt", number(status, "retryAttempt")),
        Field::text("outcome", text(status, "lastOutcome")),
        Field::text("error", text(status, "lastErrorCode")),
    ]);
    fields.extend(DETAIL_TEXT.iter().map(|(key, field)| {
        Field::text(*key, text(status, field).filter(|value| !value.is_empty()))
    }));
    fields
}

pub(super) fn served(value: &Value) -> Document {
    Document::record(
        "served",
        vec![
            Field::text("target", text(value, "target")),
            Field::text("subject", text(value, "subject")),
            Field::text("issuer", text(value, "issuer")),
            Field::text("dns_names", joined(value, "dnsNames").as_deref()),
            Field::text("not_before", text(value, "notBefore")),
            Field::text("not_after", text(value, "notAfter")),
            Field::text("expires", text(value, "expires")),
            Field::text("serial", text(value, "serial")),
            Field::text("sha256", text(value, "fingerprintSha256")),
            Field::text("protocol", text(value, "protocol")),
            Field::number("chain_length", number(value, "chainLength")),
            Field::flag("verified", flag(value, "verified")),
            Field::text("verification_error", text(value, "verificationError")),
        ],
    )
}

fn watcher_fields(watcher: &Value) -> Record {
    let health = watcher.get("health");
    let health = health
        .and_then(Value::as_str)
        .or_else(|| health.and_then(|health| text(health, "state")));
    vec![
        Field::text("health", health),
        Field::count("backend_errors", number(watcher, "backendErrors")),
        Field::count("rescans", number(watcher, "rescans")),
        Field::count(
            "reconciliation_failures",
            number(watcher, "reconciliationFailures"),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{detail_at, list_at};

    const NOW: i64 = 1_893_456_000;

    fn inventory() -> serde_json::Value {
        json!({ "certificates": [
            {
                "name": "www", "source": "files", "dnsNames": ["www.example.test"], "developmentOnly": false,
                "status": { "expiresAt": "Jan  3 00:00:00 2030 GMT", "lastOutcome": "loaded" }
            },
            {
                "name": "api", "source": "acme_managed", "dnsNames": ["api.example.test"], "developmentOnly": false,
                "status": { "notAfter": "Dec 30 12:00:00 2029 GMT", "notAfterUnixSeconds": 1_893_240_000, "paused": false }
            },
        ] })
    }

    #[test]
    fn the_list_shows_the_expiry_in_utc_and_the_time_left() {
        let plain = list_at(&inventory(), NOW).plain();
        assert!(plain.contains("name=www"), "{plain}");
        assert!(plain.contains("expires=2030-01-03T00:00:00Z"), "{plain}");
        assert!(plain.contains("left=\"in 2d00h\""), "{plain}");
        assert!(plain.contains("expires=2029-12-29T12:00:00Z"), "{plain}");
    }

    #[test]
    fn an_expired_certificate_says_how_long_ago() {
        let past = list_at(&inventory(), NOW).plain();
        assert!(past.contains("left=\"expired 2d12h ago\""), "{past}");
    }

    #[test]
    fn the_detail_lists_the_renewal_state() {
        let table = detail_at(&inventory(), NOW).table().stdout;
        assert!(table.contains("expires"), "{table}");
        assert!(table.contains("paused"), "{table}");
    }
}
