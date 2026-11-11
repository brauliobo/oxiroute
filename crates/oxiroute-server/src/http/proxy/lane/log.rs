//! The access line of a request the lane served: the classic record, built from the lane's own facts.

use std::time::Duration;

use http::Method;
use log::warn;

use super::proxy::{Forward, Progress};
use crate::http::{
    action::AccessLog,
    proxy::{
        HttpRequestContext,
        access_record::{Exchange, access_line, idempotency_header},
    },
};

pub(super) fn write(
    log: &AccessLog,
    ctx: &HttpRequestContext,
    forward: &Forward<'_>,
    progress: &Progress,
    elapsed: Duration,
) {
    let (buffer, head) = (forward.buffer, forward.head);
    let method = Method::from_bytes(forward.request.method(buffer))
        .expect("the route was selected for this method");
    let idempotency = idempotency_header(ctx).and_then(|name| {
        let name = name.as_str().as_bytes();
        head.headers()
            .iter()
            .find(|header| header.name(buffer).eq_ignore_ascii_case(name))
            .map(|header| header.value(buffer))
    });
    let exchange = Exchange {
        method: &method,
        idempotency,
        protocol: "http/1.1",
        bytes_received: progress.received() as u64,
        bytes_sent: progress.sent as u64,
    };
    let record = access_line(
        ctx,
        &exchange,
        progress.status,
        elapsed,
        log.logged_service(),
    );
    if let Err(error) = log.write_with(|line| {
        record.write_json(line);
        Ok(())
    }) {
        warn!("HTTP access log write failed: {error}");
    }
}
