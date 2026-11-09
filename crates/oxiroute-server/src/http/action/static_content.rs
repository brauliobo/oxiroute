//! Bytes of a static file: held in memory when small, read in chunks from the open descriptor otherwise.

use std::{io, os::unix::fs::FileExt as _};

use rustix::io::ReadWriteFlags;

#[allow(clippy::wildcard_imports)]
use super::*;

/// Files up to this size are read whole once and served from memory.
pub(crate) const MEMORY_FILE_BYTES: u64 = 128 * 1024;
const CHUNK_BYTES: u64 = 64 * 1024;

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
            Self::Open(file) => Source::Open(Arc::new(file)),
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
    Open(Arc<File>),
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
        let length = usize::try_from(self.remaining.min(CHUNK_BYTES)).expect("chunk bound");
        let chunk = match &self.source {
            Source::Memory(body) => {
                let start = usize::try_from(self.offset).expect("memory offset");
                body.slice(start..start + length)
            }
            Source::Open(file) => read_chunk(file, self.offset, length).await?,
        };
        self.offset += length as u64;
        self.remaining -= length as u64;
        Ok(Some(chunk))
    }
}

/// Pages already cached are copied on the calling thread; a read that would wait on the disk moves to the
/// blocking pool, so a slow volume never stalls the worker.
async fn read_chunk(file: &Arc<File>, offset: u64, length: usize) -> io::Result<Bytes> {
    let mut chunk = vec![0; length];
    let read = match preadv2_nowait(file, &mut chunk, offset) {
        Ok(read) => read,
        Err(Errno::AGAIN | Errno::OPNOTSUPP) => 0,
        Err(error) => return Err(error.into()),
    };
    if read == length {
        return Ok(chunk.into());
    }
    let file = Arc::clone(file);
    tokio::task::spawn_blocking(move || {
        file.read_exact_at(&mut chunk[read..], offset + read as u64)
            .map(|()| chunk.into())
    })
    .await
    .map_err(io::Error::other)?
}

fn preadv2_nowait(file: &File, chunk: &mut [u8], offset: u64) -> Result<usize, Errno> {
    rustix::io::preadv2(
        file,
        &mut [io::IoSliceMut::new(chunk)],
        offset,
        ReadWriteFlags::NOWAIT,
    )
}
