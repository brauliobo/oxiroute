//! Bytes of a static file: held in memory when small, read in chunks from the open descriptor otherwise (or sent by
//! the kernel when the pages are known to be in memory).

use std::{collections::VecDeque, io, os::unix::fs::FileExt as _};

use bytes::BytesMut;
use pingora::utils::chunk_pool::{self, CHUNK_BYTES};
use rustix::io::ReadWriteFlags;

#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) use super::static_kernel::KernelSlice;

/// Files up to this size are read whole once and served from memory.
pub(crate) const MEMORY_FILE_BYTES: u64 = 128 * 1024;
/// Files up to this size on a file system that cannot say whether a read would wait are read once when they are
/// opened, so that the kernel can send them from memory for the validity window of the cache.
pub(crate) const WARM_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Chunks read per blocking-pool hop when the file system cannot answer a read from its cache without waiting.
const WINDOW_CHUNKS: usize = 2;

#[derive(Clone, Debug)]
pub(crate) enum StaticContent {
    Memory(Bytes),
    /// An open file. `warm`: it was read whole a moment ago on a file system that cannot tell whether a read would
    /// wait, so its pages are taken to be in memory.
    Open {
        file: Arc<File>,
        warm: bool,
    },
}

impl StaticContent {
    pub(crate) const fn open(file: Arc<File>) -> Self {
        Self::Open { file, warm: false }
    }

    /// Reads a small open file into memory and a medium one into the page cache, if the file system cannot say
    /// whether the pages are there; larger files and files already in memory are returned as they are.
    pub(super) fn load(self, size: u64) -> Result<Self, StaticServeError> {
        let Self::Open { file, .. } = self else {
            return Ok(self);
        };
        if size > MEMORY_FILE_BYTES {
            if size <= WARM_FILE_BYTES && super::static_kernel::needs_warming(&file) {
                warm(&file, size).map_err(|_| StaticServeError::Unavailable)?;
                return Ok(Self::Open { file, warm: true });
            }
            return Ok(Self::open(file));
        }
        let mut body = vec![0; usize::try_from(size).map_err(|_| StaticServeError::TooLarge)?];
        file.read_exact_at(&mut body, 0)
            .map_err(|_| StaticServeError::Unavailable)?;
        Ok(Self::Memory(body.into()))
    }

    pub(crate) fn into_reader(self, start: u64, length: u64) -> StaticReader {
        let source = match self {
            Self::Memory(body) => Source::Memory(body),
            Self::Open { file, warm } => Source::Open(OpenFile::new(file, warm)),
        };
        StaticReader {
            source,
            offset: start,
            remaining: length,
        }
    }
}

enum Source {
    Memory(Bytes),
    Open(OpenFile),
}

/// A file read from its descriptor. Chunks come from the page cache on the calling thread while the file system
/// can answer without waiting; after the first read that cannot, the rest is read a window at a time on the
/// blocking pool and handed out in chunks.
struct OpenFile {
    file: Arc<File>,
    ahead: VecDeque<Bytes>,
    blocking: bool,
    /// Whether the file was read whole when it was opened: the kernel may send it on a file system with no
    /// non-blocking reads.
    warm: bool,
    /// Whether the rest of the file may still be sent by the kernel (cleared when the connection cannot or a page
    /// is not cached).
    kernel: bool,
}

impl OpenFile {
    fn new(file: Arc<File>, warm: bool) -> Self {
        Self {
            file,
            ahead: VecDeque::new(),
            blocking: false,
            warm,
            kernel: true,
        }
    }

    async fn read(&mut self, offset: u64, remaining: u64) -> io::Result<Bytes> {
        if self.ahead.is_empty() && !self.blocking {
            if let Some(chunk) = cached_chunk(&self.file, offset, remaining)? {
                return Ok(chunk);
            }
            self.blocking = true;
        }
        if self.ahead.is_empty() {
            self.ahead = read_window(&self.file, offset, remaining).await?;
        }
        Ok(self
            .ahead
            .pop_front()
            .expect("a window holds at least one chunk"))
    }
}

/// Yields `length` bytes from `start` in chunks of at most [`CHUNK_BYTES`].
pub(crate) struct StaticReader {
    source: Source,
    offset: u64,
    remaining: u64,
}

impl StaticReader {
    /// The next part of the file the kernel can send straight from the page cache, if there is one: a large enough
    /// range of an open file that is cached and not yet read into chunks.
    pub(crate) fn kernel_slice(&mut self) -> Option<KernelSlice> {
        let Source::Open(open) = &mut self.source else {
            return None;
        };
        if !open.kernel || open.blocking || !open.ahead.is_empty() {
            return None;
        }
        let slice = KernelSlice::next(&open.file, self.offset, self.remaining, open.warm);
        open.kernel = slice.is_some();
        slice
    }

    /// Stops using the kernel for the rest of the file (the connection cannot take it).
    pub(crate) fn without_kernel(&mut self) {
        if let Source::Open(open) = &mut self.source {
            open.kernel = false;
        }
    }

    /// Accounts for `sent` bytes the kernel sent from the start of the last slice.
    pub(crate) fn advance(&mut self, sent: u64) {
        self.offset += sent;
        self.remaining -= sent;
    }

    pub(crate) const fn is_done(&self) -> bool {
        self.remaining == 0
    }

    pub(crate) async fn next_chunk(&mut self) -> io::Result<Option<Bytes>> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let length = chunk_length(self.remaining);
        let chunk = match &mut self.source {
            Source::Memory(body) => {
                let start = usize::try_from(self.offset).expect("memory offset");
                body.slice(start..start + length)
            }
            Source::Open(open) => open.read(self.offset, self.remaining).await?,
        };
        self.offset += length as u64;
        self.remaining -= length as u64;
        Ok(Some(chunk))
    }
}

/// Reads the whole file once, so that its pages are in memory.
fn warm(file: &File, size: u64) -> io::Result<()> {
    let mut buffer = vec![0; CHUNK_BYTES];
    let mut offset = 0;
    while offset < size {
        let length = buffer
            .len()
            .min(usize::try_from(size - offset).unwrap_or(usize::MAX));
        file.read_exact_at(&mut buffer[..length], offset)?;
        offset += length as u64;
    }
    Ok(())
}

fn chunk_length(remaining: u64) -> usize {
    usize::try_from(remaining).map_or(CHUNK_BYTES, |left| left.min(CHUNK_BYTES))
}

/// A pooled buffer of `length` readable bytes: pooled buffers come back empty.
fn filled_chunk(length: usize) -> BytesMut {
    let mut chunk = chunk_pool::take();
    chunk.resize(length, 0);
    chunk
}

/// The next chunk if the page cache holds all of it; `None` when the read would wait on the disk or the file system
/// has no non-blocking reads.
fn cached_chunk(file: &File, offset: u64, remaining: u64) -> io::Result<Option<Bytes>> {
    let length = chunk_length(remaining);
    let mut chunk = filled_chunk(length);
    let read = rustix::io::preadv2(
        file,
        &mut [io::IoSliceMut::new(&mut chunk[..length])],
        offset,
        ReadWriteFlags::NOWAIT,
    );
    match read {
        Ok(read) if read == length => Ok(Some(chunk_pool::freeze(chunk, 0, length))),
        Ok(_) | Err(Errno::AGAIN | Errno::OPNOTSUPP) => {
            chunk_pool::recycle(chunk);
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

/// Reads up to [`WINDOW_CHUNKS`] chunks on the blocking pool. The buffers come from the pool of the calling thread,
/// which is also the thread that drops the chunks.
async fn read_window(file: &Arc<File>, offset: u64, remaining: u64) -> io::Result<VecDeque<Bytes>> {
    let file = Arc::clone(file);
    let mut buffers = Vec::with_capacity(WINDOW_CHUNKS);
    let mut left = remaining;
    while buffers.len() < WINDOW_CHUNKS && left > 0 {
        let length = chunk_length(left);
        buffers.push((filled_chunk(length), length));
        left -= length as u64;
    }
    let buffers = tokio::task::spawn_blocking(move || {
        let mut position = offset;
        for (buffer, length) in &mut buffers {
            file.read_exact_at(&mut buffer[..*length], position)?;
            position += *length as u64;
        }
        Ok::<_, io::Error>(buffers)
    })
    .await
    .map_err(io::Error::other)??;
    Ok(buffers
        .into_iter()
        .map(|(buffer, length)| chunk_pool::freeze(buffer, 0, length))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(length: u32) -> Vec<u8> {
        (0..length).map(|value| (value % 251) as u8).collect()
    }

    async fn chunks(mut reader: StaticReader) -> Vec<Bytes> {
        let mut chunks = Vec::new();
        while let Some(chunk) = reader.next_chunk().await.expect("chunk") {
            chunks.push(chunk);
        }
        chunks
    }

    #[tokio::test]
    async fn a_blocking_file_is_read_in_windows_and_handed_out_in_chunks() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("data.bin");
        let data = pattern(700_000);
        std::fs::write(&path, &data).expect("write");
        for (start, length) in [(0, 700_000), (262_100, 300_000), (699_999, 1)] {
            let mut reader = StaticContent::open(Arc::new(File::open(&path).expect("open")))
                .into_reader(start, length);
            let Source::Open(open) = &mut reader.source else {
                unreachable!()
            };
            open.blocking = true;
            let chunks = chunks(reader).await;
            assert!(chunks.iter().all(|chunk| chunk.len() <= CHUNK_BYTES));
            let from = usize::try_from(start).expect("start");
            let to = usize::try_from(start + length).expect("end");
            assert_eq!(chunks.concat(), data[from..to], "{start}+{length}");
        }
    }

    #[test]
    fn warming_reads_the_whole_file_and_a_file_shorter_than_its_size_is_an_error() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("data.bin");
        std::fs::write(&path, pattern(300_000)).expect("write");
        let file = File::open(&path).expect("open");
        assert!(warm(&file, 300_000).is_ok());
        assert!(warm(&file, 300_001).is_err());
    }

    #[test]
    fn a_warm_file_is_read_like_any_open_file() {
        let directory = tempfile::tempdir_in("/dev/shm").expect("directory in memory");
        let path = directory.path().join("data.bin");
        std::fs::write(&path, pattern(300_000)).expect("write");
        let file = Arc::new(File::open(&path).expect("open"));
        let mut warm = StaticContent::Open {
            file: file.clone(),
            warm: true,
        }
        .into_reader(0, 300_000);
        let Source::Open(open) = &warm.source else {
            unreachable!()
        };
        assert!(open.warm);
        assert!(warm.kernel_slice().is_some());
        let cold = StaticContent::open(file).into_reader(0, 300_000);
        let Source::Open(open) = &cold.source else {
            unreachable!()
        };
        assert!(!open.warm);
    }

    #[tokio::test]
    async fn a_window_past_the_end_of_the_file_is_an_error() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("short.bin");
        std::fs::write(&path, pattern(100)).expect("write");
        let file = Arc::new(File::open(&path).expect("open"));
        assert!(read_window(&file, 0, 200).await.is_err());
        let window = read_window(&file, 50, 50).await.expect("window");
        assert_eq!(
            window.into_iter().flatten().collect::<Vec<u8>>(),
            pattern(100)[50..]
        );
    }
}
