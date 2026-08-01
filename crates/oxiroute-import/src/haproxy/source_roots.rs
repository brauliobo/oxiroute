use std::{
    fs, io,
    ops::Deref,
    path::{Path, PathBuf},
};

use crate::{
    Diagnostic, DiagnosticStage, E_SOURCE_CHANGED, E_SOURCE_IO, E_SOURCE_LIMIT,
    MAX_AGGREGATE_SOURCE_BYTES, MAX_GLOB_MATCHES, MAX_SOURCE_BYTES, MAX_SOURCE_FILES, Report,
    Severity, SourceFile, SourceId,
    source::{FileFingerprint, StableReadFailure, read_stable_file, stable_file_changed},
};

/// Resource bounds applied while expanding and snapshotting `HAProxy` source roots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HaproxyLoadLimits {
    pub max_source_bytes: usize,
    pub max_source_files: usize,
    pub max_aggregate_source_bytes: usize,
    pub max_directory_entries: usize,
}

impl Default for HaproxyLoadLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: MAX_SOURCE_BYTES,
            max_source_files: MAX_SOURCE_FILES,
            max_aggregate_source_bytes: MAX_AGGREGATE_SOURCE_BYTES,
            max_directory_entries: MAX_GLOB_MATCHES,
        }
    }
}

/// One loaded occurrence produced by an ordered `HAProxy` `-f` source root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedSource {
    /// The original root's zero-based position in the repeated `-f` arguments.
    pub root_ordinal: usize,
    /// This selected file's zero-based position in the fully expanded file sequence.
    pub file_ordinal: usize,
    /// The selected path. This retains non-UTF-8 platform path data.
    pub path: PathBuf,
    /// An immutable snapshot of the file bytes for this occurrence.
    pub source: SourceFile,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RootLoadFailure {
    SourceIo,
    SourceChanged,
    SourceSizeLimit,
    SourceFileLimit,
    AggregateSourceLimit,
    DirectoryEntryLimit,
    UnsupportedFileType,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RootLoadOutcome {
    Loaded { file_count: usize },
    Failed(RootLoadFailure),
    NotAttempted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootLoadDecision {
    pub root_ordinal: usize,
    pub path: PathBuf,
    pub outcome: RootLoadOutcome,
}

/// Complete result of evaluating repeated `-f` roots in order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LoadedRoots {
    pub sources: Vec<LoadedSource>,
    pub decisions: Vec<RootLoadDecision>,
}

impl LoadedRoots {
    #[must_use]
    pub fn complete(&self) -> bool {
        self.decisions
            .iter()
            .all(|decision| matches!(decision.outcome, RootLoadOutcome::Loaded { .. }))
    }
}

impl Deref for LoadedRoots {
    type Target = [LoadedSource];

    fn deref(&self) -> &Self::Target {
        &self.sources
    }
}

/// Loads repeated `HAProxy` `-f` roots in argument order.
///
/// File roots produce one occurrence. Directory roots produce their direct, non-hidden `.cfg`
/// regular-file children sorted by the raw platform filename encoding. Repeated roots and paths
/// are deliberately not deduplicated.
#[must_use]
pub fn load_roots<P: AsRef<Path>>(roots: &[P]) -> Report<LoadedRoots> {
    load_roots_with_limits(roots, HaproxyLoadLimits::default())
}

/// Loads repeated roots with explicit resource bounds.
#[must_use]
pub fn load_roots_with_limits<P: AsRef<Path>>(
    roots: &[P],
    limits: HaproxyLoadLimits,
) -> Report<LoadedRoots> {
    load_roots_inner(roots, limits, || {})
}

fn load_roots_inner<P, F>(
    roots: &[P],
    limits: HaproxyLoadLimits,
    before_recheck: F,
) -> Report<LoadedRoots>
where
    P: AsRef<Path>,
    F: FnOnce(),
{
    let mut loader = Loader::new(limits);

    let mut decisions = Vec::with_capacity(roots.len());
    let mut failed = false;
    for (root_ordinal, root) in roots.iter().enumerate() {
        let path = root.as_ref().to_path_buf();
        if failed {
            decisions.push(RootLoadDecision {
                root_ordinal,
                path,
                outcome: RootLoadOutcome::NotAttempted,
            });
            continue;
        }

        let source_count = loader.sources.len();
        let outcome = match loader.load_root(root_ordinal, root.as_ref()) {
            Ok(()) => RootLoadOutcome::Loaded {
                file_count: loader.sources.len() - source_count,
            },
            Err(failure) => {
                failed = true;
                RootLoadOutcome::Failed(failure)
            }
        };
        decisions.push(RootLoadDecision {
            root_ordinal,
            path,
            outcome,
        });
    }

    before_recheck();
    for root_ordinal in loader.final_recheck() {
        failed = true;
        if let Some(decision) = decisions.get_mut(root_ordinal) {
            decision.outcome = RootLoadOutcome::Failed(RootLoadFailure::SourceChanged);
        }
    }
    Report::new(
        LoadedRoots {
            sources: if failed { Vec::new() } else { loader.sources },
            decisions,
        },
        loader.diagnostics,
    )
}

struct Loader {
    limits: HaproxyLoadLimits,
    next_file_ordinal: usize,
    aggregate_source_bytes: usize,
    source_file_limit_reported: bool,
    sources: Vec<LoadedSource>,
    source_observations: Vec<SourceObservation>,
    directory_observations: Vec<DirectoryObservation>,
    diagnostics: Vec<Diagnostic>,
}

impl Loader {
    fn new(limits: HaproxyLoadLimits) -> Self {
        Self {
            limits,
            next_file_ordinal: 0,
            aggregate_source_bytes: 0,
            source_file_limit_reported: false,
            sources: Vec::new(),
            source_observations: Vec::new(),
            directory_observations: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn load_root(&mut self, root_ordinal: usize, root: &Path) -> Result<(), RootLoadFailure> {
        let metadata = match fs::metadata(root) {
            Ok(metadata) => metadata,
            Err(error) => {
                self.root_error(root_ordinal, root, "inspect", &error);
                return Err(RootLoadFailure::SourceIo);
            }
        };

        if metadata.is_file() {
            self.load_file(root_ordinal, root.to_path_buf())
        } else if metadata.is_dir() {
            self.load_directory(root_ordinal, root)
        } else {
            self.diagnostics.push(Diagnostic::new(
                E_SOURCE_IO,
                Severity::Error,
                DiagnosticStage::Source,
                format!(
                    "HAProxy source root {root_ordinal} `{}` is neither a regular file nor a directory",
                    root.display()
                ),
            ));
            Err(RootLoadFailure::UnsupportedFileType)
        }
    }

    fn load_directory(&mut self, root_ordinal: usize, root: &Path) -> Result<(), RootLoadFailure> {
        let paths = match selected_directory_files(root, self.limits.max_directory_entries) {
            Ok(paths) => paths,
            Err(DirectoryReadFailure::Io(error)) => {
                self.root_error(root_ordinal, root, "read directory", &error);
                return Err(RootLoadFailure::SourceIo);
            }
            Err(DirectoryReadFailure::EntryLimit) => {
                self.push_source_limit(format!(
                    "HAProxy source root {root_ordinal} `{}` exceeds the directory entry work limit of {}",
                    root.display(),
                    self.limits.max_directory_entries
                ));
                return Err(RootLoadFailure::DirectoryEntryLimit);
            }
        };
        self.directory_observations.push(DirectoryObservation {
            root_ordinal,
            path: root.to_path_buf(),
            files: paths.clone(),
        });

        for path in paths {
            self.load_file(root_ordinal, path)?;
        }
        Ok(())
    }

    fn load_file(&mut self, root_ordinal: usize, path: PathBuf) -> Result<(), RootLoadFailure> {
        let file_ordinal = self.next_file_ordinal;
        let Some(next_file_ordinal) = self.next_file_ordinal.checked_add(1) else {
            self.push_source_limit(
                "HAProxy expanded source occurrence count exceeds the supported ordinal range",
            );
            return Err(RootLoadFailure::SourceFileLimit);
        };
        self.next_file_ordinal = next_file_ordinal;

        let Some(next_source_count) = self.sources.len().checked_add(1) else {
            if !self.source_file_limit_reported {
                self.source_file_limit_reported = true;
                self.push_source_limit(
                    "HAProxy expanded source file count exceeds the supported count range",
                );
            }
            return Err(RootLoadFailure::SourceFileLimit);
        };
        if next_source_count > self.limits.max_source_files {
            if !self.source_file_limit_reported {
                self.source_file_limit_reported = true;
                self.push_source_limit(format!(
                    "HAProxy expanded source file count exceeds the maximum of {} occurrences",
                    self.limits.max_source_files
                ));
            }
            return Err(RootLoadFailure::SourceFileLimit);
        }

        let Ok(source_id) = u32::try_from(file_ordinal) else {
            self.diagnostics.push(Diagnostic::new(
                E_SOURCE_LIMIT,
                Severity::Error,
                DiagnosticStage::Source,
                "HAProxy expanded source count exceeds the supported source identifier range",
            ));
            return Err(RootLoadFailure::SourceFileLimit);
        };
        let source_id = SourceId::new(source_id);

        let snapshot = match read_stable_file(&path, self.limits.max_source_bytes) {
            Ok(read) => read,
            Err(StableReadFailure::TooLarge) => {
                self.diagnostics.push(Diagnostic::new(
                    E_SOURCE_LIMIT,
                    Severity::Error,
                    DiagnosticStage::Source,
                    format!(
                        "HAProxy source root {root_ordinal}, file {file_ordinal} `{}` exceeds the maximum size of {} bytes",
                        path.display(),
                        self.limits.max_source_bytes
                    ),
                ));
                return Err(RootLoadFailure::SourceSizeLimit);
            }
            Err(StableReadFailure::Changed) => {
                self.diagnostics.push(Diagnostic::new(
                    E_SOURCE_CHANGED,
                    Severity::Error,
                    DiagnosticStage::Source,
                    format!(
                        "HAProxy source root {root_ordinal}, file {file_ordinal} `{}` changed while it was being read",
                        path.display()
                    ),
                ));
                return Err(RootLoadFailure::SourceChanged);
            }
            Err(StableReadFailure::Io(error)) => {
                self.file_error(root_ordinal, Some(file_ordinal), &path, "read", &error);
                return Err(RootLoadFailure::SourceIo);
            }
        };
        let bytes = snapshot.bytes;
        let fingerprint = snapshot.fingerprint;

        let Some(next_aggregate_source_bytes) =
            self.aggregate_source_bytes.checked_add(bytes.len())
        else {
            self.push_source_limit(format!(
                "HAProxy aggregate source size exceeds the maximum of {} bytes",
                self.limits.max_aggregate_source_bytes
            ));
            return Err(RootLoadFailure::AggregateSourceLimit);
        };
        if next_aggregate_source_bytes > self.limits.max_aggregate_source_bytes {
            self.push_source_limit(format!(
                "HAProxy aggregate source size exceeds the maximum of {} bytes",
                self.limits.max_aggregate_source_bytes
            ));
            return Err(RootLoadFailure::AggregateSourceLimit);
        }

        let source_index = self.sources.len();
        self.sources.push(LoadedSource {
            root_ordinal,
            file_ordinal,
            source: SourceFile::from_path(source_id, path.clone(), bytes),
            path,
        });
        self.aggregate_source_bytes = next_aggregate_source_bytes;
        self.source_observations.push(SourceObservation {
            source_index,
            fingerprint,
        });
        Ok(())
    }

    fn final_recheck(&mut self) -> Vec<usize> {
        let mut changed_roots = Vec::new();
        for observation in &self.directory_observations {
            let changed = match selected_directory_files(
                &observation.path,
                self.limits.max_directory_entries,
            ) {
                Ok(files) => files != observation.files,
                Err(_) => true,
            };
            if changed {
                changed_roots.push(observation.root_ordinal);
                self.diagnostics.push(Diagnostic::new(
                    E_SOURCE_CHANGED,
                    Severity::Error,
                    DiagnosticStage::Source,
                    format!(
                        "HAProxy source directory root {} `{}` changed while sources were being loaded",
                        observation.root_ordinal,
                        observation.path.display()
                    ),
                ));
            }
        }

        for observation in &self.source_observations {
            let loaded = &self.sources[observation.source_index];
            let changed = stable_file_changed(
                &loaded.path,
                self.limits.max_source_bytes,
                loaded.source.bytes(),
                &observation.fingerprint,
            );
            if changed {
                changed_roots.push(loaded.root_ordinal);
                self.diagnostics.push(
                    Diagnostic::new(
                        E_SOURCE_CHANGED,
                        Severity::Error,
                        DiagnosticStage::Source,
                        format!(
                            "HAProxy source root {}, file {} `{}` changed while sources were being loaded",
                            loaded.root_ordinal,
                            loaded.file_ordinal,
                            loaded.path.display()
                        ),
                    )
                    .with_primary_span(loaded.source.full_span()),
                );
            }
        }
        changed_roots.sort_unstable();
        changed_roots.dedup();
        changed_roots
    }

    fn root_error(&mut self, root_ordinal: usize, path: &Path, action: &str, error: &io::Error) {
        self.diagnostics.push(Diagnostic::new(
            E_SOURCE_IO,
            Severity::Error,
            DiagnosticStage::Source,
            format!(
                "cannot {action} HAProxy source root {root_ordinal} `{}`: {error}",
                path.display()
            ),
        ));
    }

    fn file_error(
        &mut self,
        root_ordinal: usize,
        file_ordinal: Option<usize>,
        path: &Path,
        action: &str,
        error: &io::Error,
    ) {
        let occurrence = file_ordinal.map_or_else(
            || format!("root {root_ordinal}"),
            |file_ordinal| format!("root {root_ordinal}, file {file_ordinal}"),
        );
        self.diagnostics.push(Diagnostic::new(
            E_SOURCE_IO,
            Severity::Error,
            DiagnosticStage::Source,
            format!(
                "cannot {action} HAProxy source {occurrence} `{}`: {error}",
                path.display()
            ),
        ));
    }

    fn push_source_limit(&mut self, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic::new(
            E_SOURCE_LIMIT,
            Severity::Error,
            DiagnosticStage::Source,
            message,
        ));
    }
}

#[derive(Default)]
struct DirectoryObservation {
    root_ordinal: usize,
    path: PathBuf,
    files: Vec<PathBuf>,
}

#[derive(Default)]
struct SourceObservation {
    source_index: usize,
    fingerprint: FileFingerprint,
}

enum DirectoryReadFailure {
    EntryLimit,
    Io(io::Error),
}

fn selected_directory_files(
    root: &Path,
    max_entries: usize,
) -> Result<Vec<PathBuf>, DirectoryReadFailure> {
    let directory = fs::read_dir(root).map_err(DirectoryReadFailure::Io)?;
    let mut entries = Vec::with_capacity(max_entries.min(256));
    for entry in directory {
        if entries.len() == max_entries {
            return Err(DirectoryReadFailure::EntryLimit);
        }
        entries.push(entry.map_err(DirectoryReadFailure::Io)?);
    }
    entries.sort_by(|left, right| {
        left.file_name()
            .as_encoded_bytes()
            .cmp(right.file_name().as_encoded_bytes())
    });

    let mut files = Vec::new();
    for entry in entries {
        if is_selected_name(&entry.file_name())
            && entry
                .metadata()
                .map_err(DirectoryReadFailure::Io)?
                .is_file()
        {
            files.push(entry.path());
        }
    }
    Ok(files)
}

fn is_selected_name(name: &std::ffi::OsStr) -> bool {
    let bytes = name.as_encoded_bytes();
    !bytes.starts_with(b".") && bytes.ends_with(b".cfg")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use crate::E_SOURCE_CHANGED;

    use super::{HaproxyLoadLimits, load_roots_inner};

    #[test]
    fn final_recheck_detects_source_changes() {
        let directory = tempdir().expect("temporary directory");
        let source = directory.path().join("haproxy.cfg");
        fs::write(&source, b"frontend before\n").expect("initial source");

        let report = load_roots_inner(
            std::slice::from_ref(&source),
            HaproxyLoadLimits::default(),
            || {
                fs::write(&source, b"backend after\n").expect("change source");
            },
        );

        assert!(
            report
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code() == E_SOURCE_CHANGED)
        );
    }

    #[test]
    fn final_recheck_detects_directory_changes() {
        let directory = tempdir().expect("temporary directory");
        fs::write(directory.path().join("a.cfg"), b"global\n").expect("initial source");

        let report = load_roots_inner(&[directory.path()], HaproxyLoadLimits::default(), || {
            fs::write(directory.path().join("b.cfg"), b"defaults\n").expect("add source");
        });

        assert!(
            report
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code() == E_SOURCE_CHANGED)
        );
    }
}
