use super::*;

pub(super) enum PollAttempt<T> {
    Complete(T),
    Pending(HttpResponse),
}

pub(super) fn poll_acme<T>(
    clock: &dyn Clock,
    identity: &str,
    poll: &PollPolicy,
    mut request: impl FnMut() -> Result<PollAttempt<T>, AcmeError>,
) -> Result<T, AcmeError> {
    let (max_attempts, mut delay, max_delay) = bounded_poll_policy(poll);
    for attempt in 0..max_attempts {
        poll_not_cancelled(poll)?;
        if clock.now_unix_seconds() > poll.deadline_unix_seconds {
            return Err(AcmeError::PollTimeout);
        }
        let response = match request()? {
            PollAttempt::Complete(value) => return Ok(value),
            PollAttempt::Pending(response) => response,
        };
        poll_not_cancelled(poll)?;
        if attempt + 1 == max_attempts {
            return Err(AcmeError::PollTimeout);
        }
        let now = clock.now_unix_seconds();
        let effective_delay = retry_after(&response, now)?
            .unwrap_or_else(|| jittered_delay(delay, max_delay, identity, attempt));
        if now.saturating_add(effective_delay) > poll.deadline_unix_seconds {
            return Err(AcmeError::PollTimeout);
        }
        poll_not_cancelled(poll)?;
        clock.sleep_seconds(effective_delay);
        delay = effective_delay.saturating_mul(2).min(max_delay);
    }
    Err(AcmeError::PollTimeout)
}

fn poll_not_cancelled(poll: &PollPolicy) -> Result<(), AcmeError> {
    if poll
        .cancellation
        .as_ref()
        .is_some_and(Dns01Cancellation::is_cancelled)
    {
        Err(AcmeError::Cancelled)
    } else {
        Ok(())
    }
}

pub(super) fn bounded_poll_policy(poll: &PollPolicy) -> (usize, u64, u64) {
    let max_attempts = poll.max_attempts.min(MAX_POLL_ATTEMPTS);
    let max_delay = poll.max_delay_seconds.min(MAX_POLL_DELAY_SECONDS);
    let initial_delay = poll.initial_delay_seconds.min(max_delay);
    (max_attempts, initial_delay, max_delay)
}

pub(super) fn jittered_delay(base: u64, max_delay: u64, identity: &str, attempt: usize) -> u64 {
    if base == 0 || max_delay == 0 {
        return 0;
    }
    let digest = Sha256::digest(format!("{identity}:{attempt}").as_bytes());
    let jitter_limit = base.min(5);
    let jitter = u64::from(digest[0]) % (jitter_limit + 1);
    base.saturating_add(jitter).min(max_delay)
}

pub(super) fn retry_after(
    response: &HttpResponse,
    now_unix_seconds: u64,
) -> Result<Option<u64>, AcmeError> {
    let Some(value) = response.header("retry-after") else {
        return Ok(None);
    };
    let seconds = if let Ok(seconds) = value.parse::<u64>() {
        seconds
    } else {
        let date = httpdate::parse_http_date(value).map_err(|_| AcmeError::InvalidRetryAfter)?;
        date.duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| AcmeError::InvalidRetryAfter)?
            .as_secs()
            .saturating_sub(now_unix_seconds)
    };
    if seconds > MAX_POLL_DELAY_SECONDS {
        return Err(AcmeError::InvalidRetryAfter);
    }
    Ok(Some(seconds))
}
