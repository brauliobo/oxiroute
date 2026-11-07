#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) struct WorkerMetadata {
    arguments: Vec<OsString>,
    environment: WorkerEnvironment,
}

impl WorkerMetadata {
    pub(super) fn new(
        arguments: Vec<OsString>,
        environment: WorkerEnvironment,
    ) -> Result<Self, WorkerMetadataError> {
        if arguments.len() > MAX_WORKER_ARGUMENTS {
            return Err(WorkerMetadataError::TooManyArguments {
                actual: arguments.len(),
                maximum: MAX_WORKER_ARGUMENTS,
            });
        }
        if environment.len() > MAX_WORKER_ENVIRONMENT {
            return Err(WorkerMetadataError::TooManyEnvironmentEntries {
                actual: environment.len(),
                maximum: MAX_WORKER_ENVIRONMENT,
            });
        }
        let mut total = 0_usize;
        for value in arguments
            .iter()
            .chain(environment.iter().flat_map(|(key, value)| [key, value]))
        {
            let length = value.as_bytes().len();
            if length > MAX_WORKER_METADATA_ITEM_BYTES {
                return Err(WorkerMetadataError::ItemTooLarge {
                    actual: length,
                    maximum: MAX_WORKER_METADATA_ITEM_BYTES,
                });
            }
            total = total.saturating_add(length);
        }
        if total > MAX_WORKER_METADATA_BYTES {
            return Err(WorkerMetadataError::MetadataTooLarge {
                actual: total,
                maximum: MAX_WORKER_METADATA_BYTES,
            });
        }
        if environment.iter().any(|(key, _)| {
            key.as_bytes().is_empty()
                || key.as_bytes().contains(&0)
                || key.as_bytes().contains(&b'=')
        }) || environment
            .iter()
            .any(|(_, value)| value.as_bytes().contains(&0))
            || arguments.iter().any(|value| value.as_bytes().contains(&0))
        {
            return Err(WorkerMetadataError::InvalidEnvironmentOrArgument);
        }
        Ok(Self {
            arguments,
            environment,
        })
    }

    pub(super) fn into_parts(self) -> (Vec<OsString>, WorkerEnvironment) {
        (self.arguments, self.environment)
    }
}

/// Public, non-secret identity carried in every authenticated frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkerIdentity {
    /// Runtime instance token.
    pub instance: InstanceToken,
    /// Service generation.
    pub generation: GenerationId,
    /// Application protocol version.
    pub protocol: u16,
}

/// A resolved worker executable plus explicitly configured non-secret launch metadata.
///
/// Arguments and environment are encoded into the launcher's visible argv. They are bounded and
/// must never contain credentials, nonces, tokens, or other secrets.
#[derive(Clone, Debug)]
pub struct WorkerCommand {
    pub(super) program: PathBuf,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    pub(super) require_cgroup_containment: bool,
}

impl WorkerCommand {
    /// Resolves an executable before the sanitized launcher environment is created.
    ///
    /// Absolute and path-containing inputs are canonicalized directly. Bare names are resolved
    /// against the caller's current `PATH`, then stored as an absolute canonical path.
    ///
    /// # Errors
    ///
    /// Returns an error when the executable cannot be resolved to an executable regular file.
    pub fn new(program: impl AsRef<OsStr>) -> Result<Self, ExecutableError> {
        Ok(Self {
            program: resolve_executable(program.as_ref())?,
            args: Vec::new(),
            env: Vec::new(),
            require_cgroup_containment: false,
        })
    }

    /// Appends non-secret worker mode metadata as one argument.
    #[must_use]
    pub fn arg(mut self, argument: impl AsRef<OsStr>) -> Self {
        self.args.push(argument.as_ref().to_owned());
        self
    }

    /// Adds one explicitly configured non-secret environment entry.
    #[must_use]
    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.env
            .push((key.as_ref().to_owned(), value.as_ref().to_owned()));
        self
    }

    /// Allowlists one variable from the parent environment when it is present.
    #[must_use]
    pub fn inherit_env(mut self, key: impl AsRef<OsStr>) -> Self {
        let key = key.as_ref();
        if let Some(value) = env::var_os(key) {
            self.env.push((key.to_owned(), value));
        }
        self
    }

    /// Requires delegated cgroup-v2 containment for this worker process tree.
    ///
    /// Spawning fails before the launcher is created when a contained worker cgroup cannot be
    /// acquired. Commands that do not set this requirement retain process-group fallback.
    #[must_use]
    pub fn require_cgroup_containment(mut self) -> Self {
        self.require_cgroup_containment = true;
        self
    }

    pub(super) fn into_launcher_command(
        self,
        launcher: &Path,
        endpoint: SeqpacketEndpoint,
        cgroup_path: Option<&Path>,
    ) -> Result<Command, WorkerMetadataError> {
        let metadata = WorkerMetadata::new(self.args, self.env)?;
        let (arguments, environment) = metadata.into_parts();
        let mut command = Command::new(launcher);
        command
            .arg(self.program)
            .arg(WORKER_METADATA_VERSION)
            .arg(if cgroup_path.is_some() { "1" } else { "0" });
        if let Some(path) = cgroup_path {
            let bytes = path.as_os_str().as_bytes();
            if bytes.len() > MAX_WORKER_METADATA_ITEM_BYTES {
                return Err(WorkerMetadataError::ItemTooLarge {
                    actual: bytes.len(),
                    maximum: MAX_WORKER_METADATA_ITEM_BYTES,
                });
            }
            if bytes.contains(&0) {
                return Err(WorkerMetadataError::InvalidEnvironmentOrArgument);
            }
            command.arg(encode_hex(bytes));
        }
        command
            .arg(arguments.len().to_string())
            .args(arguments.iter().map(|value| encode_hex(value.as_bytes())))
            .arg(environment.len().to_string());
        for (key, value) in environment {
            command
                .arg(encode_hex(key.as_bytes()))
                .arg(encode_hex(value.as_bytes()));
        }
        command
            .env_clear()
            .stdin(Stdio::from(endpoint.into_owned_fd()))
            .process_group(0);
        Ok(command)
    }
}
