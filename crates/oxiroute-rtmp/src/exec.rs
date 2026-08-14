use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

pub(crate) const MAX_EXEC_NAME_BYTES: usize = 64;
pub(crate) const MAX_EXEC_ARGUMENTS: usize = 64;
pub(crate) const MAX_EXEC_ARGUMENT_BYTES: usize = 4_096;
pub(crate) const MAX_EXEC_ARGV_BYTES: usize = 16 * 1024;
pub(crate) const MAX_EXEC_ENVIRONMENT: usize = 32;
pub(crate) const MAX_EXEC_ENV_NAME_BYTES: usize = 128;
pub(crate) const MAX_EXEC_ENV_VALUE_BYTES: usize = 4_096;
pub(crate) const MAX_EXEC_ENV_BYTES: usize = 16 * 1024;
pub(crate) const MAX_EXEC_TIMEOUT: Duration = Duration::from_hours(24);
pub(crate) const MAX_EXEC_SHUTDOWN_TIMEOUT: Duration = Duration::from_mins(1);
pub(crate) const MAX_EXEC_PROCESSES: usize = 256;
pub(crate) const MAX_EXEC_QUEUE_MESSAGES: usize = 65_536;
pub(crate) const MAX_EXEC_QUEUE_BYTES: usize = 1024 * 1024 * 1024;
pub(crate) const MAX_EXEC_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_EXEC_RESPAWN_DELAY: Duration = Duration::from_mins(5);
pub(crate) const MAX_EXEC_RESPAWNS: usize = 64;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ExecMode {
    #[default]
    Command,
    Transcode,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ExecTrigger {
    #[default]
    Publisher,
    PublishDone,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ExecFilesystemPolicy {
    #[default]
    WorkingDirectory,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ExecNetworkPolicy {
    /// Run inside a Linux network namespace when the host permits it.
    #[default]
    Disabled,
    /// Explicitly opt into the host network namespace.
    Inherited,
}

#[derive(Clone, Eq, PartialEq)]
pub struct ExecEnvironment {
    name: String,
    value: String,
}

impl fmt::Debug for ExecEnvironment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecEnvironment")
            .field("name", &self.name)
            .field("value", &"<redacted>")
            .finish()
    }
}

impl ExecEnvironment {
    /// Creates one static environment entry. Values are never included in debug or audit output.
    ///
    /// # Errors
    ///
    /// Returns an error for a malformed or oversized environment entry.
    pub fn new(
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, ExecProfileError> {
        let name = name.into();
        let value = value.into();
        validate_environment(&name, &value)?;
        Ok(Self { name, value })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecLimits {
    pub max_queue_messages: usize,
    pub max_queue_bytes: usize,
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
    pub timeout: Duration,
    pub shutdown_timeout: Duration,
    pub max_processes: usize,
    pub respawn_delay: Duration,
    pub max_respawns: usize,
}

impl ExecLimits {
    /// Creates bounded process, queue, output, timeout, and respawn limits.
    ///
    /// # Errors
    ///
    /// Returns an error when a limit is zero or exceeds its hard ceiling.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        max_queue_messages: usize,
        max_queue_bytes: usize,
        max_stdout_bytes: usize,
        max_stderr_bytes: usize,
        timeout: Duration,
        shutdown_timeout: Duration,
        max_processes: usize,
        respawn_delay: Duration,
        max_respawns: usize,
    ) -> Result<Self, ExecProfileError> {
        if max_queue_messages == 0 || max_queue_messages > MAX_EXEC_QUEUE_MESSAGES {
            return Err(ExecProfileError::InvalidLimit("max_queue_messages"));
        }
        if max_queue_bytes == 0 || max_queue_bytes > MAX_EXEC_QUEUE_BYTES {
            return Err(ExecProfileError::InvalidLimit("max_queue_bytes"));
        }
        if max_stdout_bytes == 0 || max_stdout_bytes > MAX_EXEC_OUTPUT_BYTES {
            return Err(ExecProfileError::InvalidLimit("max_stdout_bytes"));
        }
        if max_stderr_bytes == 0 || max_stderr_bytes > MAX_EXEC_OUTPUT_BYTES {
            return Err(ExecProfileError::InvalidLimit("max_stderr_bytes"));
        }
        if timeout.is_zero() || timeout > MAX_EXEC_TIMEOUT {
            return Err(ExecProfileError::InvalidLimit("timeout"));
        }
        if shutdown_timeout.is_zero() || shutdown_timeout > MAX_EXEC_SHUTDOWN_TIMEOUT {
            return Err(ExecProfileError::InvalidLimit("shutdown_timeout"));
        }
        if max_processes == 0 || max_processes > MAX_EXEC_PROCESSES {
            return Err(ExecProfileError::InvalidLimit("max_processes"));
        }
        if respawn_delay.is_zero() || respawn_delay > MAX_EXEC_RESPAWN_DELAY {
            return Err(ExecProfileError::InvalidLimit("respawn_delay"));
        }
        if max_respawns > MAX_EXEC_RESPAWNS {
            return Err(ExecProfileError::InvalidLimit("max_respawns"));
        }
        Ok(Self {
            max_queue_messages,
            max_queue_bytes,
            max_stdout_bytes,
            max_stderr_bytes,
            timeout,
            shutdown_timeout,
            max_processes,
            respawn_delay,
            max_respawns,
        })
    }
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum ExecProfileError {
    #[error("exec profile name is invalid")]
    InvalidName,
    #[error("exec profile application is invalid")]
    InvalidApplication,
    #[error("exec profile executable path is invalid")]
    InvalidExecutable,
    #[error("exec profile working directory is invalid")]
    InvalidWorkingDirectory,
    #[error("exec profile contains an unsupported shell executable")]
    ShellExecutable,
    #[error("exec profile arguments are invalid")]
    InvalidArguments,
    #[error("exec profile environment is invalid")]
    InvalidEnvironment,
    #[error("exec profile limit `{0}` is invalid")]
    InvalidLimit(&'static str),
    #[error("transcode profiles may only start for a publisher")]
    InvalidTranscodeTrigger,
}

#[derive(Clone, Eq, PartialEq)]
pub struct ExecProfile {
    name: Arc<str>,
    application: Arc<str>,
    mode: ExecMode,
    trigger: ExecTrigger,
    executable: PathBuf,
    arguments: Arc<[String]>,
    environment: Arc<[ExecEnvironment]>,
    working_directory: PathBuf,
    filesystem: ExecFilesystemPolicy,
    network: ExecNetworkPolicy,
    limits: ExecLimits,
    respawn: bool,
}

impl fmt::Debug for ExecProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecProfile")
            .field("name", &self.name)
            .field("application", &self.application)
            .field("mode", &self.mode)
            .field("trigger", &self.trigger)
            .field("executable", &"<redacted>")
            .field(
                "arguments",
                &format_args!("<{} redacted>", self.arguments.len()),
            )
            .field(
                "environment",
                &format_args!("<{} redacted>", self.environment.len()),
            )
            .field("working_directory", &"<redacted>")
            .field("filesystem", &self.filesystem)
            .field("network", &self.network)
            .field("limits", &self.limits)
            .field("respawn", &self.respawn)
            .finish()
    }
}

impl ExecProfile {
    /// Creates one exact executable profile. The executable is passed directly to the OS process
    /// API; it is never parsed as a shell command.
    ///
    /// # Errors
    ///
    /// Returns an error when the profile or any bound is invalid.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: impl Into<String>,
        application: impl Into<String>,
        mode: ExecMode,
        trigger: ExecTrigger,
        executable: PathBuf,
        arguments: impl IntoIterator<Item = String>,
        environment: impl IntoIterator<Item = ExecEnvironment>,
        working_directory: PathBuf,
        filesystem: ExecFilesystemPolicy,
        network: ExecNetworkPolicy,
        limits: ExecLimits,
        respawn: bool,
    ) -> Result<Self, ExecProfileError> {
        let name: String = name.into();
        let application: String = application.into();
        if name.is_empty()
            || name.len() > MAX_EXEC_NAME_BYTES
            || name.trim() != name
            || name.chars().any(char::is_control)
        {
            return Err(ExecProfileError::InvalidName);
        }
        if application.is_empty()
            || application.trim() != application
            || application.chars().any(char::is_control)
        {
            return Err(ExecProfileError::InvalidApplication);
        }
        validate_absolute_path(&executable, false)
            .then_some(())
            .ok_or(ExecProfileError::InvalidExecutable)?;
        validate_absolute_path(&working_directory, true)
            .then_some(())
            .ok_or(ExecProfileError::InvalidWorkingDirectory)?;
        if executable
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(is_shell_executable)
        {
            return Err(ExecProfileError::ShellExecutable);
        }
        if mode == ExecMode::Transcode && trigger != ExecTrigger::Publisher {
            return Err(ExecProfileError::InvalidTranscodeTrigger);
        }
        let arguments: Vec<_> = arguments.into_iter().collect();
        if arguments.len() > MAX_EXEC_ARGUMENTS
            || arguments.iter().any(|argument| {
                argument.len() > MAX_EXEC_ARGUMENT_BYTES
                    || argument
                        .bytes()
                        .any(|byte| byte == 0 || byte.is_ascii_control())
            })
            || arguments_byte_count(&arguments) > MAX_EXEC_ARGV_BYTES
        {
            return Err(ExecProfileError::InvalidArguments);
        }
        let environment: Vec<_> = environment.into_iter().collect();
        if environment.len() > MAX_EXEC_ENVIRONMENT
            || environment_byte_count(&environment) > MAX_EXEC_ENV_BYTES
        {
            return Err(ExecProfileError::InvalidEnvironment);
        }
        let mut names = Vec::with_capacity(environment.len());
        for entry in &environment {
            if !valid_environment_name(entry.name())
                || is_forbidden_environment_name(entry.name())
                || names.iter().any(|name| *name == entry.name())
            {
                return Err(ExecProfileError::InvalidEnvironment);
            }
            names.push(entry.name());
        }
        Ok(Self {
            name: name.into(),
            application: application.into(),
            mode,
            trigger,
            executable,
            arguments: arguments.into(),
            environment: environment.into(),
            working_directory,
            filesystem,
            network,
            limits,
            respawn,
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn application(&self) -> &str {
        &self.application
    }

    #[must_use]
    pub const fn mode(&self) -> ExecMode {
        self.mode
    }

    #[must_use]
    pub const fn trigger(&self) -> ExecTrigger {
        self.trigger
    }

    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    #[must_use]
    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }

    #[must_use]
    pub(crate) fn environment(&self) -> &[ExecEnvironment] {
        &self.environment
    }

    #[must_use]
    pub fn working_directory(&self) -> &Path {
        &self.working_directory
    }

    #[must_use]
    pub const fn filesystem(&self) -> ExecFilesystemPolicy {
        self.filesystem
    }

    #[must_use]
    pub const fn network(&self) -> ExecNetworkPolicy {
        self.network
    }

    #[must_use]
    pub const fn limits(&self) -> ExecLimits {
        self.limits
    }

    #[must_use]
    pub const fn respawn(&self) -> bool {
        self.respawn
    }
}

fn validate_absolute_path(path: &Path, directory: bool) -> bool {
    let Some(value) = path.to_str() else {
        return false;
    };
    value.starts_with('/')
        && !value.is_empty()
        && !value.ends_with('/')
        && value.len() <= 4_096
        && (!directory || value != "/")
        && !value
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
        && value.strip_prefix('/').is_some_and(|value| {
            value
                .split('/')
                .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
        })
}

fn validate_environment(name: &str, value: &str) -> Result<(), ExecProfileError> {
    if name.len() > MAX_EXEC_ENV_NAME_BYTES
        || !valid_environment_name(name)
        || is_forbidden_environment_name(name)
        || value.len() > MAX_EXEC_ENV_VALUE_BYTES
        || value
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return Err(ExecProfileError::InvalidEnvironment);
    }
    Ok(())
}

fn valid_environment_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'A'..=b'Z' | b'_'))
        && bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn is_forbidden_environment_name(name: &str) -> bool {
    matches!(
        name,
        "PATH" | "IFS" | "SHELL" | "LD_PRELOAD" | "LD_LIBRARY_PATH"
    ) || name.starts_with("LD_")
        || name.starts_with("DYLD_")
}

fn is_shell_executable(name: &str) -> bool {
    matches!(
        name,
        "sh" | "bash" | "dash" | "zsh" | "fish" | "cmd" | "cmd.exe" | "powershell"
    )
}

fn arguments_byte_count(arguments: &[String]) -> usize {
    arguments
        .iter()
        .try_fold(0_usize, |total, argument| {
            total.checked_add(argument.len() + 1)
        })
        .unwrap_or(usize::MAX)
}

fn environment_byte_count(environment: &[ExecEnvironment]) -> usize {
    environment
        .iter()
        .try_fold(0_usize, |total, entry| {
            total
                .checked_add(entry.name().len() + 1)
                .and_then(|total| total.checked_add(entry.value().len() + 1))
        })
        .unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn limits() -> ExecLimits {
        ExecLimits::new(
            8,
            64 * 1024,
            64 * 1024,
            64 * 1024,
            Duration::from_secs(5),
            Duration::from_secs(1),
            2,
            Duration::from_millis(10),
            2,
        )
        .expect("test limits are valid")
    }

    fn profile(
        executable: &str,
        mode: ExecMode,
        network: ExecNetworkPolicy,
        environment: Vec<ExecEnvironment>,
    ) -> ExecProfile {
        ExecProfile::new(
            "capture",
            "live",
            mode,
            ExecTrigger::Publisher,
            PathBuf::from(executable),
            Vec::<String>::new(),
            environment,
            PathBuf::from("/tmp"),
            ExecFilesystemPolicy::WorkingDirectory,
            network,
            limits(),
            false,
        )
        .expect("test profile is valid")
    }

    #[test]
    fn accepts_bounded_absolute_paths() {
        let profile = profile(
            "/usr/bin/cat",
            ExecMode::Command,
            ExecNetworkPolicy::Inherited,
            vec![ExecEnvironment::new("CAPTURE_MODE", "raw").unwrap()],
        );
        assert_eq!(profile.executable(), Path::new("/usr/bin/cat"));
        assert_eq!(profile.working_directory(), Path::new("/tmp"));
    }

    #[test]
    fn rejects_traversal_and_shell_paths() {
        assert_eq!(
            ExecProfile::new(
                "capture",
                "live",
                ExecMode::Command,
                ExecTrigger::Publisher,
                PathBuf::from("/usr/bin/../cat"),
                Vec::<String>::new(),
                Vec::<ExecEnvironment>::new(),
                PathBuf::from("/tmp"),
                ExecFilesystemPolicy::WorkingDirectory,
                ExecNetworkPolicy::Inherited,
                limits(),
                false,
            )
            .unwrap_err(),
            ExecProfileError::InvalidExecutable
        );
        assert_eq!(
            ExecProfile::new(
                "capture",
                "live",
                ExecMode::Command,
                ExecTrigger::Publisher,
                PathBuf::from("/bin/sh"),
                Vec::<String>::new(),
                Vec::<ExecEnvironment>::new(),
                PathBuf::from("/tmp"),
                ExecFilesystemPolicy::WorkingDirectory,
                ExecNetworkPolicy::Inherited,
                limits(),
                false,
            )
            .unwrap_err(),
            ExecProfileError::ShellExecutable
        );
    }

    #[test]
    fn rejects_duplicate_environment_names() {
        let environment = ExecEnvironment::new("CAPTURE_MODE", "raw").unwrap();
        let error = ExecProfile::new(
            "capture",
            "live",
            ExecMode::Command,
            ExecTrigger::Publisher,
            PathBuf::from("/usr/bin/cat"),
            Vec::<String>::new(),
            vec![environment.clone(), environment],
            PathBuf::from("/tmp"),
            ExecFilesystemPolicy::WorkingDirectory,
            ExecNetworkPolicy::Inherited,
            limits(),
            false,
        )
        .unwrap_err();
        assert_eq!(error, ExecProfileError::InvalidEnvironment);
    }

    #[test]
    fn debug_output_redacts_process_inputs() {
        let profile = ExecProfile::new(
            "capture",
            "live",
            ExecMode::Command,
            ExecTrigger::Publisher,
            PathBuf::from("/usr/bin/cat"),
            vec!["secret-argument".into()],
            vec![ExecEnvironment::new("CAPTURE_MODE", "secret-value").unwrap()],
            PathBuf::from("/tmp"),
            ExecFilesystemPolicy::WorkingDirectory,
            ExecNetworkPolicy::Inherited,
            limits(),
            false,
        )
        .unwrap();
        let debug = format!("{profile:?}");
        assert!(!debug.contains("/usr/bin/cat"));
        assert!(!debug.contains("secret-argument"));
        assert!(!debug.contains("secret-value"));
    }
}
