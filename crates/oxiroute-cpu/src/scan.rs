//! Shared byte classes; their consumers retain the grammar and interpretation of each match.

use crate::{Level, level};

pub(crate) const ANY3: u8 = 0;
pub(crate) const OUTSIDE_RANGE: u8 = 1;
pub(crate) const JSON_ESCAPE: u8 = 2;
pub(crate) const HEADER_CONTROL: u8 = 3;

/// Index of the first of `needles` at or after `from`, or `buf.len().max(from)`.
// Specialize the caller's delimiter constants before the inliner weighs the scalar prefix and tail loops.
#[inline(always)]
#[allow(clippy::inline_always)]
#[must_use]
pub fn find_any3(buf: &[u8], mut from: usize, needles: [u8; 3]) -> usize {
    if buf.len().saturating_sub(from) >= 16 {
        // Handle nearby delimiters without entering the runtime-dispatched vector kernel.
        let end = from + 8;
        let found = scalar_at::<ANY3>(&buf[..end], from, needles);
        if found != end {
            return found;
        }
        from = end;
    }
    auto_at::<ANY3>(buf, from, needles)
}

/// [`find_any3`] at `level`, clamped to the hardware.
#[must_use]
pub fn find_any3_with(level: Level, buf: &[u8], from: usize, needles: [u8; 3]) -> usize {
    at::<ANY3>(level.clamp(), buf, from, needles)
}

/// Whether every byte is in the inclusive range `low..=high`. An empty slice always matches.
#[inline]
#[must_use]
pub fn ascii_range(buf: &[u8], low: u8, high: u8) -> bool {
    auto_at::<OUTSIDE_RANGE>(buf, 0, [low, high, 0]) == buf.len()
}

/// [`ascii_range`] at `level`, clamped to the hardware.
#[must_use]
pub fn ascii_range_with(level: Level, buf: &[u8], low: u8, high: u8) -> bool {
    at::<OUTSIDE_RANGE>(level.clamp(), buf, 0, [low, high, 0]) == buf.len()
}

/// First JSON string byte requiring escaping: below 0x20, a quote or a backslash; otherwise the length.
#[inline]
#[must_use]
pub fn json_escape_at(buf: &[u8]) -> usize {
    auto_at::<JSON_ESCAPE>(buf, 0, [0; 3])
}

/// [`json_escape_at`] at `level`, clamped to the hardware.
#[must_use]
pub fn json_escape_at_with(level: Level, buf: &[u8]) -> usize {
    at::<JSON_ESCAPE>(level.clamp(), buf, 0, [0; 3])
}

/// First control byte other than HTAB, or DEL, at or after `from`; otherwise `buf.len().max(from)`.
#[inline]
#[must_use]
pub fn header_value_end(buf: &[u8], from: usize) -> usize {
    runtime_at::<HEADER_CONTROL>(buf, from, [0; 3])
}

/// [`header_value_end`] at `level`, clamped to the hardware.
#[must_use]
pub fn header_value_end_with(level: Level, buf: &[u8], from: usize) -> usize {
    at::<HEADER_CONTROL>(level.clamp(), buf, from, [0; 3])
}

#[inline]
fn auto_at<const KIND: u8>(buf: &[u8], from: usize, args: [u8; 3]) -> usize {
    // Below the smallest full vector, resolving a backend costs more than the scalar scan.
    if buf.len().saturating_sub(from) < 16 {
        scalar_at::<KIND>(buf, from, args)
    } else {
        runtime_at::<KIND>(buf, from, args)
    }
}

// Keep the vector switch out of the inline selector so short-input predicates specialize at their callers.
#[inline(never)]
fn runtime_at<const KIND: u8>(buf: &[u8], from: usize, args: [u8; 3]) -> usize {
    at::<KIND>(level(), buf, from, args)
}

#[inline]
fn at<const KIND: u8>(level: Level, buf: &[u8], from: usize, args: [u8; 3]) -> usize {
    #[cfg(target_arch = "x86_64")]
    {
        crate::simd::scan::at::<KIND>(level, buf, from, args)
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = level;
        scalar_at::<KIND>(buf, from, args)
    }
}

#[inline]
pub(crate) fn scalar_at<const KIND: u8>(buf: &[u8], from: usize, args: [u8; 3]) -> usize {
    if KIND == HEADER_CONTROL {
        return scalar_header_at(buf, from);
    }
    let Some(rest) = buf.get(from..) else {
        return from;
    };
    from + rest
        .iter()
        .position(|&b| matches::<KIND>(b, args))
        .unwrap_or(rest.len())
}

fn scalar_header_at(buf: &[u8], from: usize) -> usize {
    let mut end = from;
    loop {
        end = crate::scalar::control_at(buf, end);
        if buf.get(end) != Some(&b'\t') {
            return end;
        }
        end += 1;
    }
}

fn matches<const KIND: u8>(b: u8, args: [u8; 3]) -> bool {
    match KIND {
        ANY3 => args.contains(&b),
        OUTSIDE_RANGE => b < args[0] || b > args[1],
        JSON_ESCAPE => b < 0x20 || matches!(b, b'"' | b'\\'),
        _ => unreachable!(),
    }
}
