#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn legal_job_transition(current: &JobStatus, next: &JobStatus) -> bool {
    matches!(
        (current, next),
        (JobStatus::Queued, JobStatus::Running)
            | (
                JobStatus::Running,
                JobStatus::WaitingForChallenge
                    | JobStatus::Finalizing
                    | JobStatus::Succeeded
                    | JobStatus::Failed
                    | JobStatus::Cancelled
                    | JobStatus::Paused
            )
            | (
                JobStatus::WaitingForChallenge,
                JobStatus::Finalizing
                    | JobStatus::Failed
                    | JobStatus::Cancelled
                    | JobStatus::Paused
            )
            | (
                JobStatus::Finalizing,
                JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled | JobStatus::Paused
            )
    )
}

pub(super) fn validate_job_identity(job: &JobState) -> Result<(), AcmeStateError> {
    validate_slug(&job.id)?;
    validate_slug(&job.certificate)?;
    if job.operation.is_empty()
        || job.operation.len() > MAX_SLUG_BYTES
        || !is_safe_component(&job.operation)
    {
        return Err(AcmeStateError::UnsafePath);
    }
    if job.correlation_id.as_deref().is_some_and(|correlation| {
        correlation.is_empty()
            || correlation.len() > 64
            || !correlation.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            })
    }) {
        return Err(AcmeStateError::UnsafePath);
    }
    Ok(())
}

pub(super) fn is_valid_emitted_job(job: &JobState) -> bool {
    if job.updated_at_unix_seconds < job.created_at_unix_seconds
        || job.attempt > MAX_JOB_ATTEMPTS
        || job
            .next_action_unix_seconds
            .is_some_and(|next| next < job.updated_at_unix_seconds)
        || !job
            .disk_revision
            .iter()
            .chain(job.active_revision.iter())
            .all(|revision| {
                !revision.is_empty()
                    && revision.len() <= MAX_SLUG_BYTES
                    && is_safe_component(revision)
            })
        || job.last_outcome.as_ref().is_some_and(|outcome| {
            outcome.code.is_empty()
                || outcome.code.len() > MAX_SLUG_BYTES
                || !is_safe_component(&outcome.code)
                || outcome.message.is_empty()
                || outcome.message.len() > 1_024
                || outcome.message.chars().any(char::is_control)
        })
    {
        return false;
    }

    let has_transition_output = job.next_action_unix_seconds.is_some()
        || job.disk_revision.is_some()
        || job.active_revision.is_some()
        || job.last_outcome.is_some();
    match job.status {
        JobStatus::Queued => job.attempt == 0 && !has_transition_output,
        JobStatus::Running | JobStatus::WaitingForChallenge | JobStatus::Finalizing => {
            job.attempt > 0 && !has_transition_output
        }
        JobStatus::Succeeded | JobStatus::Cancelled | JobStatus::Paused => {
            job.attempt > 0 && job.next_action_unix_seconds.is_none() && job.last_outcome.is_some()
        }
        JobStatus::Failed => job.attempt > 0 && job.last_outcome.is_some(),
    }
}

pub(super) fn validate_slug(value: &str) -> Result<(), AcmeStateError> {
    if value.is_empty()
        || value.len() > MAX_SLUG_BYTES
        || value == "."
        || value == ".."
        || !is_safe_component(value)
    {
        return Err(AcmeStateError::InvalidRevision);
    }
    Ok(())
}

pub(super) fn validate_revision(value: &str) -> Result<(), AcmeStateError> {
    if value.is_empty()
        || value.len() > MAX_SLUG_BYTES
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(AcmeStateError::InvalidRevision);
    }
    Ok(())
}

pub(super) fn is_safe_component(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}
