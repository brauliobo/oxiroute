use std::time::Duration;

use super::{Attempt, until};
use crate::cli::{error::CliError, net::Timeouts};

#[test]
fn waiting_returns_as_soon_as_the_probe_succeeds() {
    let mut attempts = 0;
    let value = until(
        "the thing",
        Timeouts::limited(Duration::from_secs(5)),
        || {
            attempts += 1;
            Ok(if attempts == 3 {
                Attempt::Done("ready")
            } else {
                Attempt::Pending("not yet".into())
            })
        },
    )
    .expect("done");
    assert_eq!((value, attempts), ("ready", 3));
}

#[test]
fn an_expired_wait_is_a_timeout_that_names_the_last_reason() {
    let error = until::<()>(
        "the reload",
        Timeouts::limited(Duration::from_millis(300)),
        || Ok(Attempt::Pending("HTTP 503".into())),
    )
    .expect_err("expired");
    let message = error.to_string();
    assert!(matches!(error, CliError::Timeout(_)), "{message}");
    assert!(
        message.contains("the reload did not complete within 300ms: HTTP 503"),
        "{message}"
    );
    assert!(message.contains("--timeout"), "{message}");
}

#[test]
fn a_probe_failure_ends_the_wait_at_once() {
    let error = until::<()>(
        "the reload",
        Timeouts::limited(Duration::from_secs(5)),
        || Err(CliError::Local("boom".into())),
    )
    .expect_err("failed");
    assert_eq!(error.to_string(), "boom");
}
