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
            send_limit: 32 * 1024,
            accepts: 8,
            flush: Duration::from_mins(1),
            taskrun: Taskrun::Defer,
        }
    }
}

impl Config {
    pub(crate) fn validate(&self) -> io::Result<()> {
        let invalid =
            |reason: &'static str| Err(io::Error::new(io::ErrorKind::InvalidInput, reason));
        if !self.buffers.is_power_of_two() || self.buffers > 1 << 15 {
            return invalid("the number of provided buffers is a power of two up to 32768");
        }
        if self.buffer_size == 0 || self.buffer_size > 1 << 24 {
            return invalid("a provided buffer holds 1 byte to 16 MiB");
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
