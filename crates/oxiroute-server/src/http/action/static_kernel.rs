//! Parts of an open file that the kernel sends to a socket (`sendfile`) without reading them into user space.

use std::{fs::File, io, sync::Arc};

use rustix::{
    fs::{StatFs, fstatfs},
    io::{Errno, ReadWriteFlags},
};

/// A range that is shorter is sent through the chunk reader: the system calls of the probe and of a `sendfile` cost
/// more than the copy of a few pages.
const KERNEL_MIN_BYTES: u64 = 64 * 1024;
/// The most one `sendfile` is asked for. The pages of a slice are probed before it starts, so a smaller slice gives
/// the probe a better chance to stay true while the slice is sent.
const KERNEL_SLICE_BYTES: u64 = 1024 * 1024;
const TMPFS_MAGIC: u64 = 0x0102_1994;
const RAMFS_MAGIC: u64 = 0x8584_58f6;

pub(crate) struct KernelSlice {
    pub(crate) file: Arc<File>,
    pub(crate) offset: u64,
    pub(crate) length: usize,
}

impl KernelSlice {
    /// The slice that starts at `offset`, if at least [`KERNEL_MIN_BYTES`] remain and the page cache holds it
    /// (`warm`: the file was read whole a moment ago).
    pub(super) fn next(file: &Arc<File>, offset: u64, remaining: u64, warm: bool) -> Option<Self> {
        if remaining < KERNEL_MIN_BYTES {
            return None;
        }
        let length = remaining.min(KERNEL_SLICE_BYTES);
        resident(file, offset, length, warm).then(|| Self {
            file: Arc::clone(file),
            offset,
            length: usize::try_from(length).expect("a slice is at most one mebibyte"),
        })
    }
}

/// Whether `sendfile` of the range cannot wait for the disk: the first and the last byte are answered by a
/// non-blocking read, or the file system keeps every page in memory, or the file was just read whole. A page
/// evicted between the probe and the send makes that one `sendfile` read it from the disk on the worker thread.
fn resident(file: &File, offset: u64, length: u64, warm: bool) -> bool {
    let probe = [offset, offset + length - 1]
        .into_iter()
        .try_for_each(|position| cached(file, position));
    match probe {
        Ok(()) => true,
        Err(Errno::OPNOTSUPP) => {
            (warm || in_memory_file_system(file)) && holds(file, offset + length)
        }
        Err(_) => false,
    }
}

/// Whether the file system answers neither a non-blocking read nor a question about memory residency, so a file can
/// only be known to be in memory by reading it.
pub(super) fn needs_warming(file: &File) -> bool {
    matches!(cached(file, 0), Err(Errno::OPNOTSUPP)) && !in_memory_file_system(file)
}

fn cached(file: &File, position: u64) -> Result<(), Errno> {
    let mut byte = [0_u8];
    match rustix::io::preadv2(
        file,
        &mut [io::IoSliceMut::new(&mut byte)],
        position,
        ReadWriteFlags::NOWAIT,
    )? {
        1 => Ok(()),
        _ => Err(Errno::AGAIN),
    }
}

/// Whether the file has `end` bytes; a non-blocking read answers this for the file systems that have one.
fn holds(file: &File, end: u64) -> bool {
    file.metadata().is_ok_and(|metadata| metadata.len() >= end)
}

fn in_memory_file_system(file: &File) -> bool {
    fstatfs(file).is_ok_and(|StatFs { f_type, .. }| {
        u64::try_from(f_type).is_ok_and(|magic| [TMPFS_MAGIC, RAMFS_MAGIC].contains(&magic))
    })
}
