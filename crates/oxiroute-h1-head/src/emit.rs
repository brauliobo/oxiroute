//! Serialising the headers of a head in the order of a [`HeaderOrder`], byte for byte as the classic writers do.

use crate::edit::Edit;
use crate::fields::{LINE_IS_CLEAN, MAX_HEADERS, NAME_HAS_UPPER, TAIL_IS_CLEAN};
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
fn put_parsed(out: &mut Vec<u8>, items: &Items<'_>, index: usize) {
    let h = &items.headers[index];
    match h.class.titled() {
        Some(titled) => out.extend_from_slice(titled),
        None if h.flags & NAME_HAS_UPPER != 0 => put_lowercase(
            out,
            items.buf,
            h.name_start as usize,
            usize::from(h.name_len),
        ),
        None => out.extend_from_slice(h.name(items.buf)),
    }
    if h.flags & TAIL_IS_CLEAN != 0 {
        out.extend_from_slice(&items.buf[h.colon()..h.clean_line_end()]);
    } else {
        out.extend_from_slice(b": ");
        out.extend_from_slice(h.value(items.buf));
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
    fn for_each(self, count: usize, mut visit: impl FnMut(u8)) {
        match self {
            Self::Arrival => (0..count as u8).for_each(visit),
            Self::Map(order) => order.for_each(&mut visit),
        }
    }
}

/// Writes `name: value\r\n` for each item of `walk` whose class is not in `skip` (a mask of [`crate::Class::bit`],
/// never `Other`). Consecutive scanned lines that are already what the classic writer emits are copied as one run.
pub(crate) fn emit(out: &mut Vec<u8>, items: &Items<'_>, walk: Walk<'_>, skip: u32) {
    let mut run: Option<(usize, usize)> = None;
    let flush = |out: &mut Vec<u8>, run: &mut Option<(usize, usize)>| {
        if let Some((first, last)) = run.take() {
            let start = items.headers[first].name_start as usize;
            out.extend_from_slice(&items.buf[start..items.headers[last].clean_line_end()]);
        }
    };
    walk.for_each(items.headers.len(), |item| {
        let index = usize::from(item);
        if index >= MAX_HEADERS {
            flush(out, &mut run);
            let edit = &items.edits[index - MAX_HEADERS];
            if skip & edit.name().class.bit() == 0 {
                put_added(out, edit);
            }
            return;
        }
        let h = &items.headers[index];
        if skip & h.class.bit() != 0 {
            flush(out, &mut run);
        } else if h.flags & LINE_IS_CLEAN == 0 {
            flush(out, &mut run);
            put_parsed(out, items, index);
        } else if let Some((_, last)) = run.as_mut().filter(|(_, last)| *last + 1 == index) {
            *last = index;
        } else {
            flush(out, &mut run);
            run = Some((index, index));
        }
    });
    flush(out, &mut run);
}
