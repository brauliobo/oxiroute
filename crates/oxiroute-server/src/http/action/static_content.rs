//! Bytes of a static file: held in memory when small, read in chunks from the open descriptor otherwise.

use std::{io, os::unix::fs::FileExt as _};

use pingora::utils::chunk_pool::{self, CHUNK_BYTES};
use rustix::{buffer::spare_capacity, io::ReadWriteFlags};

#[allow(clippy::wildcard_imports)]
use super::*;

/// Files up to this size are read whole once and served from memory.
pub(crate) const MEMORY_FILE_BYTES: u64 = 128 * 1024;
/// Bytes read per blocking-pool hop when the file system cannot answer a read from its cache without waiting.
const WINDOW_BYTES: u64 = 256 * 1024;

#[derive(Debug)]
pub(crate) enum StaticContent {
    Memory(Bytes),
    Open(File),
}

impl StaticContent {
    /// Reads a small open file into memory; larger files and files already in memory are returned as they are.
    pub(super) fn load(self, size: u64) -> Result<Self, StaticServeError> {
        let Self::Open(file) = self else {
            return Ok(self);
        };
        if size > MEMORY_FILE_BYTES {
            return Ok(Self::Open(file));
        }
        let mut body = vec![0; usize::try_from(size).map_err(|_| StaticServeError::TooLarge)?];
        file.read_exact_at(&mut body, 0)
            .map_err(|_| StaticServeError::Unavailable)?;
        Ok(Self::Memory(body.into()))
    }

    pub(crate) fn into_reader(self, start: u64, length: u64) -> StaticReader {
        let source = match self {
            Self::Memory(body) => Source::Memory(body),
            Self::Open(file) => Source::Open(OpenFile {
                file: Arc::new(file),
                ahead: Bytes::new(),
                blocking: false,
            }),
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
    ahead: Bytes,
    blocking: bool,
}

impl OpenFile {
    async fn read(&mut self, offset: u64, length: usize, remaining: u64) -> io::Result<Bytes> {
        if self.ahead.is_empty() && !self.blocking {
            if let Some(chunk) = cached_chunk(&self.file, offset, length)? {
                return Ok(chunk);
            }
            self.blocking = true;
        }
        if self.ahead.is_empty() {
            self.ahead = read_window(&self.file, offset, remaining.min(WINDOW_BYTES)).await?;
        }
        Ok(self.ahead.split_to(length))
    }
}

/// Yields `length` bytes from `start` in chunks of at most [`CHUNK_BYTES`].
pub(crate) struct StaticReader {
    source: Source,
    offset: u64,
    remaining: u64,
}

impl StaticReader {
    pub(crate) async fn next_chunk(&mut self) -> io::Result<Option<Bytes>> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let length =
            usize::try_from(self.remaining).map_or(CHUNK_BYTES, |left| left.min(CHUNK_BYTES));
        let chunk = match &mut self.source {
            Source::Memory(body) => {
                let start = usize::try_from(self.offset).expect("memory offset");
                body.slice(start..start + length)
            }
            Source::Open(open) => open.read(self.offset, length, self.remaining).await?,
        };
        self.offset += length as u64;
        self.remaining -= length as u64;
        Ok(Some(chunk))
    }
}

/// The chunk if the page cache holds all of it; `None` when the read would wait on the disk or the file system has
/// no non-blocking reads.
fn cached_chunk(file: &File, offset: u64, length: usize) -> io::Result<Option<Bytes>> {
    let mut chunk = chunk_pool::take();
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

async fn read_window(file: &Arc<File>, offset: u64, length: u64) -> io::Result<Bytes> {
    let file = Arc::clone(file);
    let length = usize::try_from(length).expect("window bound");
    tokio::task::spawn_blocking(move || {
        let mut window = Vec::with_capacity(length);
        while window.len() < length {
            let position = offset + window.len() as u64;
            match rustix::io::pread(&*file, spare_capacity(&mut window), position) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(_) | Err(Errno::INTR) => {}
                Err(error) => return Err(error.into()),
            }
        }
        window.truncate(length);
        Ok(window.into())
    })
    .await
    .map_err(io::Error::other)?
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
            let mut reader =
                StaticContent::Open(File::open(&path).expect("open")).into_reader(start, length);
            let Source::Open(open) = &mut reader.source else {
                unreachable!()
            };
            open.blocking = true;
            let chunks = chunks(reader).await;
            assert!(chunks.iter().all(|chunk| chunk.len() <= CHUNK_BYTES));
            let (from, to) = (start as usize, (start + length) as usize);
            assert_eq!(chunks.concat(), data[from..to], "{start}+{length}");
        }
    }

    #[tokio::test]
    async fn a_window_past_the_end_of_the_file_is_an_error() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("short.bin");
        std::fs::write(&path, pattern(100)).expect("write");
        let file = Arc::new(File::open(&path).expect("open"));
        assert!(read_window(&file, 0, 200).await.is_err());
        assert_eq!(
            read_window(&file, 50, 50).await.expect("window"),
            pattern(100)[50..]
        );
    }
}
