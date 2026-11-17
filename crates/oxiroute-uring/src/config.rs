use std::{io, time::Duration};

/// How the ring runs task work that completions need.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Taskrun {
    /// Task work runs only inside the wait call of the single issuing thread (kernel 6.1).
    Defer,
    /// Task work interrupts the issuing thread only when it enters the kernel (kernel 5.19).
    Cooperative,
}

#[derive(Clone, Debug)]
pub struct Config {
    /// Submission queue entries; the completion queue holds twice as many.
    pub entries: u32,
    /// Provided receive buffers shared by every socket of the ring, a power of two.
    pub buffers: u16,
    /// Bytes of one provided receive buffer.
    pub buffer_size: u32,
    /// Larger provided receive buffers for sockets that keep filling the small ones, a power of two. Pages of a
    /// buffer that never receives a byte are never resident.
    pub large_buffers: u16,
    /// Bytes of one large provided receive buffer.
    pub large_buffer_size: u32,
    /// Largest number of bytes one send stages; larger writes are accepted in parts.
    pub send_limit: usize,
    /// Accept operations kept in flight per listener.
    pub accepts: u8,
    /// How long a closed socket may wait for the peer to take an accepted send before the connection is reset.
    pub flush: Duration,
    pub taskrun: Taskrun,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            entries: 2048,
            buffers: 256,
            buffer_size: 16 * 1024,
            large_buffers: 64,
            large_buffer_size: 64 * 1024,
            send_limit: 32 * 1024,
            accepts: 8,
            flush: Duration::from_mins(1),
            taskrun: Taskrun::Defer,
        }
    }
}

const PAGE: u64 = 4096;
const SUBMISSION_ENTRY: u64 = 64;
const COMPLETION_ENTRY: u64 = 16;
const ARRAY_ENTRY: u64 = 4;
const RINGS_HEADER: u64 = 64;
const BUFFER_RING_ENTRY: u64 = 16;

impl Config {
    /// Bytes the kernel charges against `RLIMIT_MEMLOCK` for one ring of this configuration where it accounts ring
    /// memory to the user: the submission entries, the completion entries, the submission index array and the page
    /// array of each provided-buffer ring, every region rounded up to whole pages. The limit is shared by every
    /// process of the user and a closed ring returns its share only after the kernel has torn it down.
    #[must_use]
    pub fn ring_bytes(&self) -> u64 {
        let submissions = u64::from(self.entries.next_power_of_two());
        let completions = 2 * submissions;
        let entries = submissions * SUBMISSION_ENTRY;
        let rings = RINGS_HEADER + completions * COMPLETION_ENTRY + submissions * ARRAY_ENTRY;
        let pools = self
            .pools()
            .map(|(buffers, _)| (u64::from(buffers) * BUFFER_RING_ENTRY).next_multiple_of(PAGE));
        entries.next_multiple_of(PAGE) + rings.next_multiple_of(PAGE) + pools.iter().sum::<u64>()
    }

    pub(crate) const fn pools(&self) -> [(u16, u32); 2] {
        [
            (self.buffers, self.buffer_size),
            (self.large_buffers, self.large_buffer_size),
        ]
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        let invalid =
            |reason: &'static str| Err(io::Error::new(io::ErrorKind::InvalidInput, reason));
        for (buffers, size) in self.pools() {
            if !buffers.is_power_of_two() || buffers > 1 << 15 {
                return invalid("the number of provided buffers is a power of two up to 32768");
            }
            if size == 0 || size > 1 << 24 {
                return invalid("a provided buffer holds 1 byte to 16 MiB");
            }
        }
        if self.large_buffer_size < self.buffer_size {
            return invalid("a large provided buffer is at least as large as a small one");
        }
        if self.accepts == 0 || self.accepts > 16 {
            return invalid("a listener keeps 1 to 16 accepts in flight");
        }
        if self.send_limit == 0 || u32::try_from(self.send_limit).is_err() {
            return invalid("a send stages at least one byte");
        }
        Ok(())
    }
}
