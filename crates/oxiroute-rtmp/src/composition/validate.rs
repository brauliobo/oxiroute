#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn exec_error(error: &ExecProfileError) -> RtmpPrepareError {
    let field = match error {
        ExecProfileError::InvalidName => "exec.name",
        ExecProfileError::InvalidApplication => "exec.application",
        ExecProfileError::InvalidExecutable | ExecProfileError::ShellExecutable => {
            "exec.executable"
        }
        ExecProfileError::InvalidWorkingDirectory => "exec.working_directory",
        ExecProfileError::InvalidArguments => "exec.arguments",
        ExecProfileError::InvalidEnvironment => "exec.environment",
        ExecProfileError::InvalidLimit(field) => field,
        ExecProfileError::InvalidTranscodeTrigger => "exec.trigger",
    };
    value(field)
}
pub(super) fn validate_identity(value: &str, field: &'static str) -> Result<(), RtmpPrepareError> {
    if value.is_empty() || value.trim() != value || value.chars().any(char::is_control) {
        Err(RtmpPrepareError::new(RtmpPrepareCategory::Identity, field))
    } else {
        Ok(())
    }
}

pub(super) fn validate_application(
    value: &str,
    field: &'static str,
) -> Result<(), RtmpPrepareError> {
    RtmpStreamPath::validate_application(value)
        .map_err(|_| RtmpPrepareError::new(RtmpPrepareCategory::Identity, field))
}

pub(super) fn validate_stream_name(
    value: &str,
    field: &'static str,
) -> Result<(), RtmpPrepareError> {
    RtmpStreamPath::parse("application", value)
        .map(|_| ())
        .map_err(|_| RtmpPrepareError::new(RtmpPrepareCategory::Identity, field))
}
#[allow(clippy::too_many_arguments)]
pub(super) fn validate_media_bounds(
    root: &Path,
    segment: Duration,
    max_segment: Duration,
    playlist: Duration,
    max_bytes: usize,
    max_messages: usize,
    max_storage_bytes: u64,
    max_storage_files: usize,
    max_streams: usize,
) -> Result<(), RtmpPrepareError> {
    if !valid_absolute_path(root)
        || segment.is_zero()
        || max_segment < segment
        || playlist < segment
        || max_bytes == 0
        || max_messages == 0
        || max_storage_bytes == 0
        || max_storage_files == 0
        || max_streams == 0
    {
        Err(bound("media.bounds"))
    } else {
        Ok(())
    }
}

pub(super) fn valid_absolute_file_path(path: &Path) -> bool {
    valid_absolute_path(path) && path != Path::new("/")
}

pub(super) fn valid_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str().len() <= 4_096
        && path.components().all(|component| {
            matches!(
                component,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            ) && component.as_os_str().to_str().is_some_and(|value| {
                !value
                    .bytes()
                    .any(|byte| byte == 0 || byte.is_ascii_control())
            })
        })
}
pub(super) fn validate_destination(host: &str, port: u16) -> Result<(), RtmpPrepareError> {
    if host.is_empty()
        || host.len() > 253
        || host.trim() != host
        || host.chars().any(char::is_control)
        || port == 0
    {
        Err(value("relay.destination"))
    } else {
        Ok(())
    }
}
pub(super) fn bound(field: &'static str) -> RtmpPrepareError {
    RtmpPrepareError::new(RtmpPrepareCategory::Bound, field)
}
pub(super) fn value(field: &'static str) -> RtmpPrepareError {
    RtmpPrepareError::new(RtmpPrepareCategory::Value, field)
}
