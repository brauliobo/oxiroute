//! Serialising the headers of a head in the order of a [`HeaderOrder`], byte for byte as the classic writers do.

use crate::edit::Edit;
use crate::fields::{Header, LINE_IS_CLEAN, NAME_HAS_UPPER, TAIL_IS_CLEAN};
use crate::order::{HeaderOrder, Items};
use alloc::vec::Vec;

const LANES: u64 = 0x0101_0101_0101_0101;

/// `word` with each ASCII capital in its bytes lowercased; bytes that are not ASCII are left alone.
fn lowercase_word(word: u64) -> u64 {
    let low7 = word & (0x7f * LANES);
    let from_a = low7 + (0x80 - u64::from(b'A')) * LANES;
    let past_z = low7 + (0x7f - u64::from(b'Z')) * LANES;
    word | ((from_a & !past_z & !word & (0x80 * LANES)) >> 2)
}

fn load_word(buf: &[u8], at: usize) -> u64 {
    let mut word = [0u8; 8];
    word.copy_from_slice(&buf[at..at + 8]);
    u64::from_le_bytes(word)
}

/// Appends the lowercase of `buf[start..start + len]`.
fn put_lowercase(out: &mut Vec<u8>, buf: &[u8], start: usize, len: usize) {
    if len <= 16 && start + 16 <= buf.len() {
        let written = out.len();
        out.extend_from_slice(&lowercase_word(load_word(buf, start)).to_le_bytes());
        out.extend_from_slice(&lowercase_word(load_word(buf, start + 8)).to_le_bytes());
        out.truncate(written + len);
    } else {
        out.extend(buf[start..start + len].iter().map(u8::to_ascii_lowercase));
    }
}

fn put_added(out: &mut Vec<u8>, edit: &Edit<'_>) {
    if let Some(value) = edit.value() {
        out.extend_from_slice(edit.name().wire);
        out.extend_from_slice(b": ");
        out.extend_from_slice(value.0);
        out.extend_from_slice(b"\r\n");
    }
}

/// A header line whose name or tail is not what the classic writer emits.
fn put_parsed(out: &mut Vec<u8>, buf: &[u8], h: &Header) {
    match h.class.titled() {
        Some(titled) => out.extend_from_slice(titled),
        None if h.flags & NAME_HAS_UPPER != 0 => {
            put_lowercase(out, buf, h.name_start as usize, usize::from(h.name_len));
        }
        None => out.extend_from_slice(h.name(buf)),
    }
    if h.flags & TAIL_IS_CLEAN != 0 {
        out.extend_from_slice(&buf[h.colon()..h.clean_line_end()]);
    } else {
        out.extend_from_slice(b": ");
        out.extend_from_slice(h.value(buf));
        out.extend_from_slice(b"\r\n");
    }
}

/// How `emit` visits the items: in arrival order, or in the order of an emulated map.
#[derive(Clone, Copy)]
pub(crate) enum Walk<'a> {
    Arrival,
    Map(&'a HeaderOrder),
}

impl Walk<'_> {
    #[inline(always)]
    fn for_each(self, count: usize, mut visit: impl FnMut(usize)) {
        match self {
            Self::Arrival => (0..count).for_each(visit),
            Self::Map(order) => order.for_each(&mut visit),
        }
    }
}

/// Consecutive scanned lines that are already what the classic writer emits, copied as one slice of the buffer.
struct Run {
    start: usize,
    end: usize,
    next: usize,
}

impl Run {
    const EMPTY: Self = Self {
        start: 0,
        end: 0,
        next: 0,
    };

    #[inline(always)]
    fn flush(&mut self, out: &mut Vec<u8>, buf: &[u8]) {
        if self.end > self.start {
            out.extend_from_slice(&buf[self.start..self.end]);
            *self = Self::EMPTY;
        }
    }

    #[inline(always)]
    fn push(&mut self, out: &mut Vec<u8>, buf: &[u8], index: usize, h: &Header) {
        if self.end == self.start || self.next != index {
            self.flush(out, buf);
            self.start = h.name_start as usize;
        }
        self.end = h.clean_line_end();
        self.next = index + 1;
    }
}

/// Writes `name: value\r\n` for each item of `walk` whose class is not in `skip` (a mask of [`crate::Class::bit`],
/// never `Other`). Consecutive scanned lines that are already what the classic writer emits are copied as one run.
pub(crate) fn emit(out: &mut Vec<u8>, items: &Items<'_>, walk: Walk<'_>, skip: u32) {
    let mut run = Run::EMPTY;
    walk.for_each(items.headers.len(), |index| {
        let Some(h) = items.headers.get(index) else {
            run.flush(out, items.buf);
            let edit = &items.edits[index - items.edit_base()];
            if skip & edit.name().class.bit() == 0 {
                put_added(out, edit);
            }
            return;
        };
        if skip & h.class.bit() != 0 {
            run.flush(out, items.buf);
        } else if h.flags & LINE_IS_CLEAN != 0 {
            run.push(out, items.buf, index, h);
        } else {
            run.flush(out, items.buf);
            put_parsed(out, items.buf, h);
        }
    });
    run.flush(out, items.buf);
}
