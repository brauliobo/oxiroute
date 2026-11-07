#[allow(clippy::wildcard_imports)]
use super::*;

#[allow(clippy::too_many_lines)]
pub(super) fn validate_rtmp_exec_profile(
    service: &str,
    applications: &[crate::model::RtmpApplication],
    profile: &mut RtmpExecProfile,
) -> Result<(), ConfigError> {
    let invalid = |field, detail| ConfigError::InvalidRtmpServicePolicy {
        service: service.into(),
        field,
        detail,
    };
    if profile.name.len() > MAX_RTMP_EXEC_NAME_BYTES {
        return Err(invalid("exec_profiles[].name", "must be at most 64 bytes"));
    }
    if !applications
        .iter()
        .any(|application| application.name == profile.application)
    {
        return Err(invalid(
            "exec_profiles[].application",
            "must reference an exact application name",
        ));
    }
    normalize_exec_path(&mut profile.executable)
        .map_err(|detail| invalid("exec_profiles[].executable", detail))?;
    normalize_absolute_directory(&mut profile.working_directory)
        .map_err(|detail| invalid("exec_profiles[].working_directory", detail))?;
    if profile.filesystem == RtmpExecFilesystemPolicy::Host {
        return Err(invalid(
            "exec_profiles[].filesystem",
            "host filesystem access is not supported",
        ));
    }
    if profile.mode == RtmpExecMode::Transcode && profile.trigger != RtmpExecTrigger::Publisher {
        return Err(invalid(
            "exec_profiles[].trigger",
            "transcode profiles require the publisher trigger",
        ));
    }
    if profile
        .executable
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(is_shell_executable_name)
    {
        return Err(invalid(
            "exec_profiles[].executable",
            "shell interpreters are not allowed",
        ));
    }
    let mut argv_bytes = 0_u64;
    if profile.arguments.len() > MAX_RTMP_EXEC_ARGUMENTS {
        return Err(invalid(
            "exec_profiles[].arguments",
            "must contain at most 64 arguments",
        ));
    }
    for argument in &profile.arguments {
        if argument.len() > MAX_RTMP_EXEC_ARGUMENT_BYTES
            || argument
                .bytes()
                .any(|byte| byte == 0 || byte.is_ascii_control())
        {
            return Err(invalid(
                "exec_profiles[].arguments",
                "arguments must be bounded UTF-8 without NUL or control bytes",
            ));
        }
        argv_bytes = argv_bytes
            .checked_add(u64::try_from(argument.len()).unwrap_or(u64::MAX))
            .and_then(|bytes| bytes.checked_add(1))
            .ok_or_else(|| invalid("exec_profiles[].arguments", "argument byte count overflow"))?;
    }
    if argv_bytes > MAX_RTMP_EXEC_ARGV_BYTES {
        return Err(invalid(
            "exec_profiles[].arguments",
            "combined argument bytes exceed 16384",
        ));
    }
    if profile.environment.len() > MAX_RTMP_EXEC_ENVIRONMENT {
        return Err(invalid(
            "exec_profiles[].environment",
            "must contain at most 32 variables",
        ));
    }
    let mut environment_bytes = 0_u64;
    let mut environment_names = HashSet::new();
    for environment in &profile.environment {
        if environment.name.len() > MAX_RTMP_EXEC_ENV_NAME_BYTES
            || !valid_exec_environment_name(&environment.name)
            || is_forbidden_exec_environment_name(&environment.name)
        {
            return Err(invalid(
                "exec_profiles[].environment[].name",
                "must be an allowed ASCII environment name",
            ));
        }
        if !environment_names.insert(&environment.name) {
            return Err(invalid(
                "exec_profiles[].environment",
                "variable names must be unique",
            ));
        }
        if environment.value.len() > MAX_RTMP_EXEC_ENV_VALUE_BYTES
            || environment
                .value
                .bytes()
                .any(|byte| byte == 0 || byte.is_ascii_control())
        {
            return Err(invalid(
                "exec_profiles[].environment[].value",
                "values must be bounded UTF-8 without NUL or control bytes",
            ));
        }
        environment_bytes = environment_bytes
            .checked_add(u64::try_from(environment.name.len()).unwrap_or(u64::MAX))
            .and_then(|bytes| bytes.checked_add(1))
            .and_then(|bytes| {
                bytes.checked_add(u64::try_from(environment.value.len()).unwrap_or(u64::MAX))
            })
            .and_then(|bytes| bytes.checked_add(1))
            .ok_or_else(|| {
                invalid(
                    "exec_profiles[].environment",
                    "environment byte count overflow",
                )
            })?;
    }
    if environment_bytes > MAX_RTMP_EXEC_ENV_BYTES {
        return Err(invalid(
            "exec_profiles[].environment",
            "combined environment bytes exceed 16384",
        ));
    }
    for (field, value, maximum, detail) in [
        (
            "exec_profiles[].timeout_ms",
            profile.timeout_ms,
            MAX_RTMP_EXEC_TIMEOUT_MS,
            "must be between 1 and 86400000",
        ),
        (
            "exec_profiles[].shutdown_timeout_ms",
            profile.shutdown_timeout_ms,
            MAX_RTMP_EXEC_SHUTDOWN_TIMEOUT_MS,
            "must be between 1 and 60000",
        ),
        (
            "exec_profiles[].max_processes",
            profile.max_processes,
            MAX_RTMP_EXEC_PROCESSES,
            "must be between 1 and 256",
        ),
        (
            "exec_profiles[].max_queue_messages",
            profile.max_queue_messages,
            MAX_RTMP_EXEC_QUEUE_MESSAGES,
            "must be between 1 and 65536",
        ),
        (
            "exec_profiles[].max_queue_bytes",
            profile.max_queue_bytes,
            MAX_RTMP_EXEC_QUEUE_BYTES,
            "must be between 1 and 1073741824",
        ),
        (
            "exec_profiles[].max_stdout_bytes",
            profile.max_stdout_bytes,
            MAX_RTMP_EXEC_STDOUT_BYTES,
            "must be between 1 and 16777216",
        ),
        (
            "exec_profiles[].max_stderr_bytes",
            profile.max_stderr_bytes,
            MAX_RTMP_EXEC_STDERR_BYTES,
            "must be between 1 and 16777216",
        ),
        (
            "exec_profiles[].respawn_delay_ms",
            profile.respawn_delay_ms,
            MAX_RTMP_EXEC_RESPAWN_DELAY_MS,
            "must be between 1 and 300000",
        ),
        (
            "exec_profiles[].max_respawns",
            profile.max_respawns,
            MAX_RTMP_EXEC_RESPAWNS,
            "must be between 0 and 64",
        ),
    ] {
        if (field != "exec_profiles[].max_respawns" && value == 0) || value > maximum {
            return Err(invalid(field, detail));
        }
    }
    Ok(())
}

fn normalize_exec_path(path: &mut PathBuf) -> Result<(), &'static str> {
    let value = path.to_str().ok_or("path must be valid UTF-8")?;
    if !value.starts_with('/') {
        return Err("path must be absolute");
    }
    if value.is_empty() || value.ends_with('/') || value.len() > 4_096 {
        return Err("path must be a bounded executable path");
    }
    if value
        .bytes()
        .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return Err("path must not contain NUL or control bytes");
    }
    if value.strip_prefix('/').is_none_or(|value| {
        value
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    }) {
        return Err("path must not contain empty, `.` or `..` segments");
    }
    *path = value.into();
    Ok(())
}

fn valid_exec_environment_name(name: &str) -> bool {
    let mut characters = name.bytes();
    matches!(characters.next(), Some(b'A'..=b'Z' | b'_'))
        && characters.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn is_forbidden_exec_environment_name(name: &str) -> bool {
    matches!(
        name,
        "PATH" | "IFS" | "SHELL" | "LD_PRELOAD" | "LD_LIBRARY_PATH"
    ) || name.starts_with("LD_")
        || name.starts_with("DYLD_")
}

fn is_shell_executable_name(name: &str) -> bool {
    matches!(
        name,
        "sh" | "bash" | "dash" | "zsh" | "fish" | "cmd" | "cmd.exe" | "powershell"
    )
}
