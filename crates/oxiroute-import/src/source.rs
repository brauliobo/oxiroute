use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

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
