//! Real inaccessible pages prove that full and masked vector loads stay within each input slice.
#![cfg(unix)]
#![allow(unsafe_code)]

use oxiroute_cpu::{
    Job, Lanes, Level, WIDTH, ascii_range_with, control_at_with, dispatch_with, find_any3_with,
    header_value_end_with, json_escape_at_with,
};

struct TokenRun<'a>(&'a [u8]);

impl Job for TokenRun<'_> {
    type Out = usize;

    #[inline(always)]
    fn run<L: Lanes>(self, lanes: L) -> usize {
        let mut at = 0;
        loop {
            let stop = lanes.token(&lanes.window(self.0, at)).stop;
            if stop != 0 {
                return at + stop.trailing_zeros() as usize;
            }
            at += WIDTH;
        }
    }
}

struct Pages {
    base: *mut libc::c_void,
    len: usize,
}

impl Pages {
    fn new() -> Self {
        // SAFETY: sysconf has no pointer arguments and querying page size has no side effects.
        let len = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap();
        // SAFETY: a new anonymous mapping; no existing mapping is replaced.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len * 3,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(base, libc::MAP_FAILED);
        // SAFETY: the middle page is page-aligned and contained in this mapping.
        assert_eq!(
            unsafe { libc::mprotect(base.byte_add(len), len, libc::PROT_READ | libc::PROT_WRITE) },
            0
        );
        Self { base, len }
    }

    fn middle(&mut self) -> &mut [u8] {
        // SAFETY: only this mutable borrow accesses the middle page, mapped readable and writable above.
        unsafe { std::slice::from_raw_parts_mut(self.base.byte_add(self.len).cast(), self.len) }
    }
}

impl Drop for Pages {
    fn drop(&mut self) {
        // SAFETY: this is the original mapping, still owned by this value.
        assert_eq!(unsafe { libc::munmap(self.base, self.len * 3) }, 0);
    }
}

#[test]
fn scanners_never_read_before_or_after_the_slice() {
    let mut pages = Pages::new();
    let middle = pages.middle();
    middle.fill(b'a');
    for level in Level::supported() {
        eprintln!("guard-page coverage: {}", level.name());
        for len in 0..=192 {
            for buf in [&middle[..len], &middle[middle.len() - len..]] {
                assert_eq!(dispatch_with(level, TokenRun(buf)), len);
                assert_eq!(json_escape_at_with(level, buf), len);
                assert!(ascii_range_with(level, buf, 32, 126));
                assert_eq!(ascii_range_with(level, buf, 0, 31), len == 0);
                for from in [0, 1, 15, 16, 31, 32, 63, 64, len, usize::MAX] {
                    assert_eq!(control_at_with(level, buf, from), len.max(from));
                    assert_eq!(header_value_end_with(level, buf, from), len.max(from));
                    assert_eq!(find_any3_with(level, buf, from, *b"/%\\"), len.max(from));
                }
            }
        }
    }
}
