//! Serialising the headers of a head in the order of a [`HeaderOrder`], byte for byte as the classic writers do.

use crate::edit::Edit;
use crate::fields::{LINE_IS_CLEAN, MAX_HEADERS, NAME_HAS_UPPER};
use crate::order::{HeaderOrder, Items};
use alloc::vec::Vec;

/// Upper bound of the bytes `emit` adds: a parsed line grows by at most the space after its colon.
pub(crate) fn size_bound(items: &Items<'_>) -> usize {
    let parsed: usize = items
        .headers
        .iter()
        .map(|h| usize::from(h.name_len) + h.value_len as usize + 4)
        .sum();
    let added: usize = items
        .edits
        .iter()
        .map(|e| e.value().map_or(0, |v| e.name().wire.len() + v.0.len() + 4))
        .sum();
    parsed + added
}

fn put_added(out: &mut Vec<u8>, edit: &Edit<'_>) {
    if let Some(value) = edit.value() {
        out.extend_from_slice(edit.name().wire);
        out.extend_from_slice(b": ");
        out.extend_from_slice(value.0);
        out.extend_from_slice(b"\r\n");
    }
}

fn put_parsed(out: &mut Vec<u8>, items: &Items<'_>, index: usize) {
    let h = &items.headers[index];
    let name = h.name(items.buf);
    match h.class.titled() {
        Some(titled) => out.extend_from_slice(titled),
        None if h.flags & NAME_HAS_UPPER != 0 => {
            out.extend(name.iter().map(u8::to_ascii_lowercase));
        }
        None => out.extend_from_slice(name),
    }
    out.extend_from_slice(b": ");
    out.extend_from_slice(h.value(items.buf));
    out.extend_from_slice(b"\r\n");
}

/// Writes `name: value\r\n` for each item in `order` whose class is not in `skip` (a mask of [`crate::Class::bit`], never `Other`).
/// Consecutive scanned lines that are already what the classic writer emits are copied as one run.
pub(crate) fn emit(out: &mut Vec<u8>, items: &Items<'_>, order: &HeaderOrder, skip: u32) {
    let mut run: Option<(usize, usize)> = None;
    let flush = |out: &mut Vec<u8>, run: &mut Option<(usize, usize)>| {
        if let Some((first, last)) = run.take() {
            let start = items.headers[first].name_start as usize;
            out.extend_from_slice(&items.buf[start..items.headers[last].clean_line_end()]);
        }
    };
    order.for_each(|item| {
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
