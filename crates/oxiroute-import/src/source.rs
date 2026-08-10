use std::{
    collections::HashMap,
    fs::{File, Metadata},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(not(unix))]
use std::time::SystemTime;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct FileFingerprint {
    length: u64,
    #[cfg(not(unix))]
    modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    modified_seconds: i64,
    #[cfg(unix)]
    modified_nanoseconds: i64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
}

impl FileFingerprint {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            length: metadata.len(),
            #[cfg(not(unix))]
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            modified_seconds: metadata.mtime(),
            #[cfg(unix)]
            modified_nanoseconds: metadata.mtime_nsec(),
            #[cfg(unix)]
            changed_seconds: metadata.ctime(),
            #[cfg(unix)]
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug)]
pub(crate) enum StableReadFailure {
    TooLarge,
    Changed,
    Io(io::Error),
}

pub(crate) struct StableFileSnapshot {
    pub(crate) bytes: Vec<u8>,
    pub(crate) fingerprint: FileFingerprint,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SourceBudget {
    pub(crate) files: usize,
    pub(crate) source_bytes: usize,
    pub(crate) aggregate_bytes: usize,
}

#[derive(Clone, Copy, Debug)]
#[allow(
    dead_code,
    reason = "product integration tests compile this internal module with only one identity mode"
)]
pub(crate) enum SourceIdentity {
    Catalog,
    Occurrence(SourceId),
}

#[derive(Clone, Copy, Debug)]
#[allow(
    dead_code,
    reason = "product integration tests compile this internal module with only one naming mode"
)]
pub(crate) enum SourceNaming {
    Anonymous,
    Path,
}

#[derive(Debug)]
pub(crate) enum SourceCatalogFailure {
    SourceFileLimit,
    SourceSizeLimit,
    AggregateSourceLimit,
    Changed,
    Io(io::Error),
}

#[derive(Clone)]
struct SourceSnapshot {
    source: SourceFile,
    path: PathBuf,
    fingerprint: FileFingerprint,
    first_include_stack: Vec<Span>,
}

pub(crate) struct SourceCatalog {
    budget: SourceBudget,
    aggregate_bytes: usize,
    snapshots: Vec<SourceSnapshot>,
    source_ids: HashMap<PathBuf, SourceId>,
}

impl SourceCatalog {
    pub(crate) fn new(budget: SourceBudget) -> Self {
        Self {
            budget,
            aggregate_bytes: 0,
            snapshots: Vec::new(),
            source_ids: HashMap::new(),
        }
    }

    pub(crate) fn source_id(&self, path: &Path) -> Option<SourceId> {
        self.source_ids.get(path).copied()
    }

    pub(crate) fn load(
        &mut self,
        path: &Path,
        first_include_stack: Vec<Span>,
        identity: SourceIdentity,
        naming: SourceNaming,
    ) -> Result<SourceFile, SourceCatalogFailure> {
        if matches!(identity, SourceIdentity::Catalog)
            && let Some(id) = self.source_id(path)
        {
            return Ok(self
                .snapshots
                .iter()
                .find(|snapshot| snapshot.source.id() == id)
                .expect("catalog source identity has a snapshot")
                .source
                .clone());
        }
        if self.snapshots.len() >= self.budget.files {
            return Err(SourceCatalogFailure::SourceFileLimit);
        }
        let id = match identity {
            SourceIdentity::Catalog => SourceId::new(
                u32::try_from(self.snapshots.len())
                    .map_err(|_| SourceCatalogFailure::SourceFileLimit)?,
            ),
            SourceIdentity::Occurrence(id) => id,
        };
        let snapshot =
            read_stable_file(path, self.budget.source_bytes).map_err(|failure| match failure {
                StableReadFailure::TooLarge => SourceCatalogFailure::SourceSizeLimit,
                StableReadFailure::Changed => SourceCatalogFailure::Changed,
                StableReadFailure::Io(error) => SourceCatalogFailure::Io(error),
            })?;
        let aggregate_bytes = self
            .aggregate_bytes
            .checked_add(snapshot.bytes.len())
            .filter(|bytes| *bytes <= self.budget.aggregate_bytes)
            .ok_or(SourceCatalogFailure::AggregateSourceLimit)?;
        let source = match naming {
            SourceNaming::Anonymous => {
                SourceFile::new(id, format!("native-source-{}", id.get()), snapshot.bytes)
            }
            SourceNaming::Path => SourceFile::from_path(id, path, snapshot.bytes),
        };
        self.aggregate_bytes = aggregate_bytes;
        if matches!(identity, SourceIdentity::Catalog) {
            self.source_ids.insert(path.to_path_buf(), id);
        }
        self.snapshots.push(SourceSnapshot {
            source: source.clone(),
            path: path.to_path_buf(),
            fingerprint: snapshot.fingerprint,
            first_include_stack,
        });
        Ok(source)
    }

    pub(crate) fn changed_snapshots(&self) -> Vec<(SourceFile, Vec<Span>)> {
        self.snapshots
            .iter()
            .filter(|snapshot| {
                stable_file_changed(
                    &snapshot.path,
                    self.budget.source_bytes,
                    snapshot.source.bytes(),
                    &snapshot.fingerprint,
                )
            })
            .map(|snapshot| {
                (
                    snapshot.source.clone(),
                    snapshot.first_include_stack.clone(),
                )
            })
            .collect()
    }
}

pub(crate) fn read_stable_file(
    path: &Path,
    max_bytes: usize,
) -> Result<StableFileSnapshot, StableReadFailure> {
    let mut file = File::open(path).map_err(StableReadFailure::Io)?;
    let before = file.metadata().map_err(StableReadFailure::Io)?;
    if !before.is_file() {
        return Err(StableReadFailure::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "source is not a regular file",
        )));
    }
    if before.len() > u64::try_from(max_bytes).unwrap_or(u64::MAX) {
        return Err(StableReadFailure::TooLarge);
    }

    let mut bytes = Vec::with_capacity(usize::try_from(before.len()).unwrap_or(max_bytes));
    file.by_ref()
        .take(
            u64::try_from(max_bytes)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        )
        .read_to_end(&mut bytes)
        .map_err(StableReadFailure::Io)?;
    if bytes.len() > max_bytes {
        return Err(StableReadFailure::TooLarge);
    }

    let after = file.metadata().map_err(StableReadFailure::Io)?;
    let before = FileFingerprint::from_metadata(&before);
    let after = FileFingerprint::from_metadata(&after);
    if before != after || after.length != u64::try_from(bytes.len()).unwrap_or(u64::MAX) {
        return Err(StableReadFailure::Changed);
    }
    Ok(StableFileSnapshot {
        bytes,
        fingerprint: after,
    })
}

pub(crate) fn stable_file_changed(
    path: &Path,
    max_bytes: usize,
    expected_bytes: &[u8],
    expected_fingerprint: &FileFingerprint,
) -> bool {
    read_stable_file(path, max_bytes).map_or(true, |snapshot| {
        snapshot.bytes != expected_bytes || snapshot.fingerprint != *expected_fingerprint
    })
}

/// Stable identity assigned to one source in an import operation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceId(u32);

impl SourceId {
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Half-open byte offsets within a source file.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ByteRange {
    start: usize,
    end: usize,
}

impl ByteRange {
    /// Creates a range containing `start..end`.
    ///
    /// # Panics
    ///
    /// Panics when `start` is greater than `end`.
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        assert!(start <= end, "byte range start must not exceed its end");
        Self { start, end }
    }

    #[must_use]
    pub const fn start(self) -> usize {
        self.start
    }

    #[must_use]
    pub const fn end(self) -> usize {
        self.end
    }

    #[must_use]
    pub const fn len(self) -> usize {
        self.end - self.start
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    #[must_use]
    pub const fn contains(self, offset: usize) -> bool {
        self.start <= offset && offset < self.end
    }
}

/// A byte range tied to its source file.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Span {
    source: SourceId,
    range: ByteRange,
}

impl Span {
    #[must_use]
    pub const fn new(source: SourceId, range: ByteRange) -> Self {
        Self { source, range }
    }

    #[must_use]
    pub const fn source(self) -> SourceId {
        self.source
    }

    #[must_use]
    pub const fn range(self) -> ByteRange {
        self.range
    }
}

/// Immutable, cheaply cloneable native source bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceFile {
    id: SourceId,
    name: Arc<str>,
    path: Option<Arc<PathBuf>>,
    bytes: Arc<[u8]>,
}

impl SourceFile {
    pub fn new(id: SourceId, name: impl Into<Arc<str>>, bytes: impl Into<Arc<[u8]>>) -> Self {
        Self {
            id,
            name: name.into(),
            path: None,
            bytes: bytes.into(),
        }
    }

    /// Creates a source retaining exact filesystem path identity, including non-UTF-8 data.
    pub fn from_path(id: SourceId, path: impl Into<PathBuf>, bytes: impl Into<Arc<[u8]>>) -> Self {
        let path = path.into();
        let name = path.to_str().map_or_else(
            || Arc::<str>::from(format!("native-source-{}", id.get())),
            Arc::<str>::from,
        );
        Self {
            id,
            name,
            path: Some(Arc::new(path)),
            bytes: bytes.into(),
        }
    }

    #[must_use]
    pub const fn id(&self) -> SourceId {
        self.id
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Exact platform path identity when this source was created from a filesystem path.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref().map(PathBuf::as_path)
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    #[must_use]
    pub fn slice(&self, range: ByteRange) -> Option<&[u8]> {
        self.bytes.get(range.start()..range.end())
    }

    #[must_use]
    pub fn span(&self, range: ByteRange) -> Option<Span> {
        (range.end() <= self.len()).then(|| Span::new(self.id, range))
    }

    #[must_use]
    pub fn full_span(&self) -> Span {
        Span::new(self.id, ByteRange::new(0, self.len()))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{StableReadFailure, read_stable_file, stable_file_changed};

    #[test]
    fn stable_file_snapshot_enforces_bounds_and_detects_replacement() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("source.conf");
        fs::write(&path, b"exact").expect("write source");

        let snapshot = read_stable_file(&path, 5).expect("exact bound");
        assert_eq!(snapshot.bytes, b"exact");
        assert!(!stable_file_changed(
            &path,
            5,
            &snapshot.bytes,
            &snapshot.fingerprint,
        ));
        assert!(matches!(
            read_stable_file(&path, 4),
            Err(StableReadFailure::TooLarge)
        ));

        fs::write(&path, b"other").expect("replace source");
        assert!(stable_file_changed(
            &path,
            5,
            &snapshot.bytes,
            &snapshot.fingerprint,
        ));
    }
}
