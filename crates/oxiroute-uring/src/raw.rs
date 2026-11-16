//! The unsafe boundary of the crate: every `unsafe` block lives in this module.
//!
//! The kernel reads and writes user memory asynchronously, after a submission returns. Three invariants make
//! that sound, and each is enforced here rather than by callers:
//!
//! 1. Memory an operation references (a send buffer, an address) moves into the flight table before its entry
//!    is queued and leaves it only when the completion of that exact operation is reaped, or after
//!    [`Queue`] has cancelled and drained everything in `Drop`; if the drain fails the memory is leaked.
//! 2. The provided-buffer ring and its data area are owned by [`Buffers`], live as long as the ring, and a
//!    buffer is read only between its completion and its return to the ring.
//! 3. Each flight has its own user data (index plus generation), so a cancellation can only match the
//!    operation it was issued for, never a later operation that reused the slot.
#![allow(unsafe_code)]

use std::{
    alloc::{Layout, alloc_zeroed, dealloc},
    io,
    mem::ManuallyDrop,
    net::SocketAddr,
    os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
    ptr::NonNull,
    sync::atomic::{AtomicU16, Ordering},
    time::Duration,
};

use io_uring::{IoUring, cqueue, opcode, squeue, types};
use rustix::{
    event::PollFlags,
    io::Errno,
    net::{RecvFlags, SendFlags, SocketFlags},
};

use crate::{addr::Address, config::Taskrun};

const BUFFER_GROUP: u16 = 0;
const RECV_POLL_FIRST: u16 = 1;
const ENTRY_WORDS: usize = 8;
const PAGE_WORDS: usize = 2048;
const TAIL_WORD: usize = 7;
const DRAIN_ROUNDS: usize = 64;

#[cfg(not(target_endian = "little"))]
compile_error!("the provided-buffer ring layout is written for little-endian targets");

#[repr(C, align(4096))]
struct Page([AtomicU16; PAGE_WORDS]);

impl Page {
    fn zeroed() -> Self {
        Self(std::array::from_fn(|_| AtomicU16::new(0)))
    }
}

pub(crate) struct Buffers {
    pages: Box<[Page]>,
    data: NonNull<u8>,
    layout: Layout,
    count: u16,
    size: u32,
    tail: u16,
}

impl Buffers {
    fn new(count: u16, size: u32) -> io::Result<Self> {
        let entries_per_page = PAGE_WORDS / ENTRY_WORDS;
        let pages = (0..usize::from(count).div_ceil(entries_per_page))
            .map(|_| Page::zeroed())
            .collect();
        let layout = Layout::from_size_align(usize::from(count) * size as usize, 4096)
            .map_err(io::Error::other)?;
        // SAFETY: the layout has a non-zero size because the configuration validated `count` and `size`.
        let data = NonNull::new(unsafe { alloc_zeroed(layout) })
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        Ok(Self {
            pages,
            data,
            layout,
            count,
            size,
            tail: 0,
        })
    }

    fn fill(&mut self) {
        for bid in 0..self.count {
            self.give(bid);
        }
    }

    fn word(&self, entry: usize, word: usize) -> &AtomicU16 {
        let per_page = PAGE_WORDS / ENTRY_WORDS;
        &self.pages[entry / per_page].0[(entry % per_page) * ENTRY_WORDS + word]
    }

    pub(crate) fn give(&mut self, bid: u16) {
        let entry = usize::from(self.tail & (self.count - 1));
        let address = (self
            .data
            .as_ptr()
            .wrapping_add(usize::from(bid) * self.size as usize) as usize)
            as u64;
        for part in 0..4 {
            let bits = (address >> (16 * part)) as u16;
            self.word(entry, part).store(bits, Ordering::Relaxed);
        }
        self.word(entry, 4)
            .store(self.size as u16, Ordering::Relaxed);
        self.word(entry, 5)
            .store((self.size >> 16) as u16, Ordering::Relaxed);
        self.word(entry, 6).store(bid, Ordering::Relaxed);
        self.tail = self.tail.wrapping_add(1);
        self.word(0, TAIL_WORD).store(self.tail, Ordering::Release);
    }

    pub(crate) fn copy_out(&self, bid: u16, from: usize, dst: &mut [u8]) -> usize {
        let size = self.size as usize;
        let count = dst.len().min(size.saturating_sub(from));
        let src = self
            .data
            .as_ptr()
            .wrapping_add(usize::from(bid) * size)
            .wrapping_add(from);
        // SAFETY: `bid < count` and `from + count <= size`, so the source range lies inside the data area; the
        // kernel finished writing this buffer before its completion was reaped and does not touch it again
        // until `give` returns it to the ring; `dst` is a distinct live allocation of `count` bytes at least.
        unsafe { std::ptr::copy_nonoverlapping(src, dst.as_mut_ptr(), count) };
        count
    }
}

impl Drop for Buffers {
    fn drop(&mut self) {
        // SAFETY: `data` was allocated with exactly this layout in `new` and is freed once, after the ring that
        // used it was unregistered or leaked together with it.
        unsafe { dealloc(self.data.as_ptr(), self.layout) };
    }
}

pub(crate) enum Memory {
    None,
    Bytes(Vec<u8>),
    Address(Box<Address>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Op {
    Plain,
    Accept,
    Cancel,
}

struct Flight {
    meta: u64,
    op: Op,
    memory: Memory,
}

#[derive(Default)]
struct Flights {
    slots: Vec<(u32, Option<Flight>)>,
    free: Vec<u32>,
    live: usize,
}

impl Flights {
    fn insert(&mut self, flight: Flight) -> u64 {
        self.live += 1;
        let index = self.free.pop().unwrap_or_else(|| {
            self.slots.push((0, None));
            u32::try_from(self.slots.len() - 1).expect("the flight table fits 32 bits")
        });
        let slot = &mut self.slots[index as usize];
        slot.1 = Some(flight);
        u64::from(slot.0) << 32 | u64::from(index)
    }

    fn remove(&mut self, key: u64) -> Option<Flight> {
        let slot = self.slots.get_mut(key as u32 as usize)?;
        if slot.0 != (key >> 32) as u32 {
            return None;
        }
        let flight = slot.1.take()?;
        slot.0 = slot.0.wrapping_add(1);
        self.free.push(key as u32);
        self.live -= 1;
        Some(flight)
    }
}

pub(crate) struct Completion {
    pub(crate) meta: u64,
    pub(crate) result: i32,
    pub(crate) buffer: Option<u16>,
    pub(crate) memory: Memory,
    pub(crate) accepted: Option<OwnedFd>,
}

pub(crate) struct Queue {
    ring: IoUring,
    flights: Flights,
    buffers: ManuallyDrop<Buffers>,
    scratch: Vec<(u64, i32, u32)>,
}

fn flag(flags: SocketFlags) -> i32 {
    flags.bits().cast_signed()
}

impl Queue {
    pub(crate) fn new(entries: u32, taskrun: Taskrun, count: u16, size: u32) -> io::Result<Self> {
        let mut builder = IoUring::builder();
        builder.setup_single_issuer();
        match taskrun {
            Taskrun::Defer => builder.setup_defer_taskrun(),
            Taskrun::Cooperative => builder.setup_coop_taskrun(),
        };
        let ring = builder.build(entries)?;
        let mut buffers = Buffers::new(count, size)?;
        // SAFETY: the page array is heap allocated, never moves, and is owned by `Queue`, which unregisters the
        // ring in `Drop` before the pages are freed (or leaks both when completions cannot be drained).
        unsafe {
            ring.submitter().register_buf_ring_with_flags(
                buffers.pages.as_ptr() as usize as u64,
                count,
                BUFFER_GROUP,
                0,
            )?;
        }
        buffers.fill();
        Ok(Self {
            ring,
            flights: Flights::default(),
            buffers: ManuallyDrop::new(buffers),
            scratch: Vec::new(),
        })
    }

    pub(crate) fn probe(&self) -> io::Result<io_uring::Probe> {
        let mut probe = io_uring::Probe::new();
        self.ring.submitter().register_probe(&mut probe)?;
        Ok(probe)
    }

    pub(crate) fn buffers(&mut self) -> &mut Buffers {
        &mut self.buffers
    }

    pub(crate) fn in_flight(&self) -> usize {
        self.flights.live
    }

    fn push(&mut self, entry: squeue::Entry, flight: Flight) -> io::Result<u64> {
        let key = self.flights.insert(flight);
        let entry = entry.user_data(key);
        loop {
            // SAFETY: the memory `entry` references was moved into the flight table above and stays there until
            // this entry's completion is reaped (invariant 1), the buffer ring outlives the ring (invariant 2).
            let pushed = unsafe { self.ring.submission().push(&entry) };
            if pushed.is_ok() {
                return Ok(key);
            }
            if let Err(error) = self.ring.submit() {
                self.flights.remove(key);
                return Err(error);
            }
        }
    }

    fn plain(&mut self, entry: squeue::Entry, meta: u64) -> io::Result<u64> {
        let flight = Flight {
            meta,
            op: Op::Plain,
            memory: Memory::None,
        };
        self.push(entry, flight)
    }

    pub(crate) fn recv(&mut self, fd: BorrowedFd<'_>, meta: u64) -> io::Result<u64> {
        let entry = opcode::Recv::new(types::Fd(fd.as_raw_fd()), std::ptr::null_mut(), 0)
            .buf_group(BUFFER_GROUP)
            .ioprio(RECV_POLL_FIRST)
            .build()
            .flags(squeue::Flags::BUFFER_SELECT);
        self.plain(entry, meta)
    }

    pub(crate) fn poll_in(&mut self, fd: BorrowedFd<'_>, meta: u64) -> io::Result<u64> {
        let entry =
            opcode::PollAdd::new(types::Fd(fd.as_raw_fd()), u32::from(PollFlags::IN.bits()))
                .build();
        self.plain(entry, meta)
    }

    pub(crate) fn send(
        &mut self,
        fd: BorrowedFd<'_>,
        bytes: Vec<u8>,
        meta: u64,
    ) -> io::Result<u64> {
        let flags = i32::from((SendFlags::NOSIGNAL.bits() | RecvFlags::WAITALL.bits()) as u16);
        let length = u32::try_from(bytes.len()).map_err(io::Error::other)?;
        let entry = opcode::Send::new(types::Fd(fd.as_raw_fd()), bytes.as_ptr(), length)
            .flags(flags)
            .build();
        let flight = Flight {
            meta,
            op: Op::Plain,
            memory: Memory::Bytes(bytes),
        };
        self.push(entry, flight)
    }

    pub(crate) fn accept(
        &mut self,
        fd: BorrowedFd<'_>,
        mut storage: Box<Address>,
        meta: u64,
    ) -> io::Result<u64> {
        storage.reset();
        let (address, length) = storage.out_pointers();
        let flags = flag(SocketFlags::NONBLOCK | SocketFlags::CLOEXEC);
        let entry = opcode::Accept::new(types::Fd(fd.as_raw_fd()), address.cast(), length.cast())
            .flags(flags)
            .build();
        let flight = Flight {
            meta,
            op: Op::Accept,
            memory: Memory::Address(storage),
        };
        self.push(entry, flight)
    }

    pub(crate) fn connect(
        &mut self,
        fd: BorrowedFd<'_>,
        peer: SocketAddr,
        meta: u64,
    ) -> io::Result<u64> {
        let storage = Box::new(Address::from_socket(peer));
        let (address, length) = storage.in_pointers();
        let entry = opcode::Connect::new(types::Fd(fd.as_raw_fd()), address.cast(), length).build();
        let flight = Flight {
            meta,
            op: Op::Plain,
            memory: Memory::Address(storage),
        };
        self.push(entry, flight)
    }

    pub(crate) fn cancel(&mut self, target: u64) -> io::Result<()> {
        let entry = opcode::AsyncCancel2::new(types::CancelBuilder::user_data(target)).build();
        let flight = Flight {
            meta: 0,
            op: Op::Cancel,
            memory: Memory::None,
        };
        self.push(entry, flight).map(drop)
    }

    pub(crate) fn wait(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        let waited = match timeout {
            Some(timeout) => {
                let span = types::Timespec::new()
                    .sec(timeout.as_secs())
                    .nsec(timeout.subsec_nanos());
                let args = types::SubmitArgs::new().timespec(&span);
                self.ring.submitter().submit_with_args(1, &args)
            }
            None => self.ring.submit_and_wait(1),
        };
        match waited {
            Ok(_) => Ok(()),
            Err(error) => match error.raw_os_error().map(Errno::from_raw_os_error) {
                Some(Errno::TIME | Errno::INTR | Errno::BUSY) => Ok(()),
                _ => Err(error),
            },
        }
    }

    pub(crate) fn reap(&mut self, out: &mut Vec<Completion>) {
        let mut completions = self.ring.completion();
        completions.sync();
        self.scratch
            .extend(completions.map(|entry| (entry.user_data(), entry.result(), entry.flags())));
        for (key, result, flags) in self.scratch.drain(..) {
            let Some(flight) = self.flights.remove(key) else {
                continue;
            };
            if flight.op == Op::Cancel {
                continue;
            }
            out.push(Completion {
                meta: flight.meta,
                result,
                buffer: cqueue::buffer_select(flags),
                memory: flight.memory,
                accepted: (flight.op == Op::Accept && result >= 0).then(|| {
                    // SAFETY: a successful accept completion carries a descriptor the kernel just created for
                    // this process; each completion is reaped once, so the descriptor gets exactly one owner.
                    unsafe { OwnedFd::from_raw_fd(result) }
                }),
            });
        }
    }

    fn drain(&mut self) {
        let mut discarded = Vec::new();
        for _ in 0..DRAIN_ROUNDS {
            if self.flights.live == 0 {
                return;
            }
            let _ = self.cancel_everything();
            let _ = self.wait(Some(Duration::from_millis(5)));
            self.reap(&mut discarded);
            discarded.clear();
        }
    }

    fn cancel_everything(&mut self) -> io::Result<()> {
        let entry = opcode::AsyncCancel2::new(types::CancelBuilder::any().all()).build();
        let flight = Flight {
            meta: 0,
            op: Op::Cancel,
            memory: Memory::None,
        };
        self.push(entry, flight).map(drop)
    }
}

impl Drop for Queue {
    fn drop(&mut self) {
        self.drain();
        if self.flights.live > 0 {
            std::mem::forget(std::mem::take(&mut self.flights));
            return;
        }
        let _ = self.ring.submitter().unregister_buf_ring(BUFFER_GROUP);
        // SAFETY: the buffers are dropped exactly once here, after the ring stopped using them; the early return
        // above leaks them instead when completions are still outstanding.
        unsafe { ManuallyDrop::drop(&mut self.buffers) };
    }
}
