//! The header block shared by requests and responses: one pass over the lines noting names, values and facts.

use crate::class::{Class, TITLED, classify, same_name};
use crate::miss::MissReason;
use crate::tables::{TOKEN, UNDERSCORE, UPPER, control_at};

/// Headers a head may carry before it is a miss (or slow).
pub const MAX_HEADERS: usize = 64;

/// The longest header name the scanners take.
pub const MAX_NAME_LEN: usize = 255;

/// Header flag: the name has an ASCII capital.
pub const NAME_HAS_UPPER: u8 = 1;
/// Header flag: the name has an underscore.
pub const NAME_HAS_UNDERSCORE: u8 = 2;
/// Header flag: the line is byte for byte what the classic writers emit (`name: value\r\n`).
pub const LINE_IS_CLEAN: u8 = 4;
/// Header flag: the line from the colon on is what the classic writers emit (`: value\r\n`), whatever the name.
pub const TAIL_IS_CLEAN: u8 = 8;

/// One header of a scanned head, as offsets into the buffer that was scanned.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Header {
    pub name_start: u32,
    pub value_start: u32,
    pub value_len: u32,
    pub name_len: u8,
    pub class: Class,
    pub flags: u8,
}

impl Header {
    /// The name as sent, in whatever case.
    #[inline]
    #[must_use]
    pub fn name<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        &buf[self.name_start as usize..self.name_start as usize + usize::from(self.name_len)]
    }

    /// The offset after this header's line when its tail is [clean](TAIL_IS_CLEAN).
    #[inline]
    pub(crate) fn clean_line_end(&self) -> usize {
        (self.value_start + self.value_len + 2) as usize
    }

    /// The offset of the colon.
    #[inline]
    pub(crate) fn colon(&self) -> usize {
        self.name_start as usize + usize::from(self.name_len)
    }

    /// The value, without the whitespace around it.
    #[inline]
    #[must_use]
    pub fn value<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        &buf[self.value_start as usize..(self.value_start + self.value_len) as usize]
    }
}

/// Why the block stopped before its blank line.
#[derive(Clone, Copy)]
pub(crate) enum Stop {
    Partial,
    Bad(MissReason),
}

/// What one pass over the header block noted.
pub(crate) struct Block {
    pub head_len: u32,
    pub count: u8,
    /// Classes seen at least once, and at least twice.
    pub seen: u32,
    pub dup: u32,
    /// The index of the first header of each class.
    pub first: [u8; 32],
    /// OR of the byte classes of all names.
    pub bits: u8,
    /// Two headers share a name, in any case.
    pub duplicate_names: bool,
}

impl Block {
    pub(crate) const fn new() -> Self {
        Self {
            head_len: 0,
            count: 0,
            seen: 0,
            dup: 0,
            first: [0; 32],
            bits: 0,
            duplicate_names: false,
        }
    }

    pub(crate) const fn underscore(&self) -> bool {
        self.bits & UNDERSCORE != 0
    }

    pub(crate) const fn upper(&self) -> bool {
        self.bits & UPPER != 0
    }
}

type Step<T> = Result<T, Stop>;

#[inline(always)]
fn bad<T>(reason: MissReason) -> Step<T> {
    Err(Stop::Bad(reason))
}

/// Offset after the CRLF at `i`.
#[inline(always)]
fn crlf(buf: &[u8], i: usize) -> Step<usize> {
    match buf.get(i) {
        Some(b'\r') => match buf.get(i + 1) {
            Some(b'\n') => Ok(i + 2),
            Some(_) => bad(MissReason::BareCr),
            None => Err(Stop::Partial),
        },
        Some(b'\n') => bad(MissReason::BareLf),
        Some(_) => bad(MissReason::Version),
        None => Err(Stop::Partial),
    }
}

/// End of the value that starts at `start`: the index of its CR, with the line known to end in CRLF.
#[inline(always)]
fn value_end(buf: &[u8], start: usize) -> Step<usize> {
    let mut end = control_at(buf, start);
    loop {
        match buf.get(end) {
            Some(b'\t') => end = control_at(buf, end + 1),
            Some(b'\r') => {
                return match buf.get(end + 1) {
                    Some(b'\n') => Ok(end),
                    Some(_) => bad(MissReason::BareCr),
                    None => Err(Stop::Partial),
                };
            }
            Some(b'\n') => return bad(MissReason::BareLf),
            Some(_) => return bad(MissReason::Control),
            None => return Err(Stop::Partial),
        }
    }
}

/// Length of the run of token bytes at the start of `line`, and the OR of their classes.
#[inline(never)]
fn name_run(line: &[u8]) -> (usize, u8) {
    let run = oxiroute_cpu::token_run(line);
    let bits = (u8::from(run.len > 0) * TOKEN)
        | (u8::from(run.upper) * UPPER)
        | (u8::from(run.underscore) * UNDERSCORE);
    (run.len, bits)
}

/// Whether the name at `index`, of class [`Class::Other`], repeats one before it.
fn repeats_other(buf: &[u8], headers: &[Header], index: usize, name: &[u8]) -> bool {
    headers[..index]
        .iter()
        .any(|h| same_name(h.name(buf), h.class, name, Class::Other))
}

/// Scans the lines from `start` to the blank line that ends the head, filling `headers` and `block`.
pub(crate) fn scan_fields(
    buf: &[u8],
    start: usize,
    headers: &mut [Header; MAX_HEADERS],
    block: &mut Block,
) -> Step<()> {
    let mut other_buckets = 0u64;
    let mut i = start;
    loop {
        let Some(line) = buf.get(i..).filter(|line| !line.is_empty()) else {
            return Err(Stop::Partial);
        };
        match line[0] {
            b'\r' => {
                block.head_len = crlf(buf, i)? as u32;
                block.duplicate_names |= block.dup != 0;
                return Ok(());
            }
            b'\n' => return bad(MissReason::BareLf),
            _ => {}
        }
        let (len, bits) = name_run(line);
        let name_end = i + len;
        match line.get(len) {
            Some(b':') if len > 0 => {}
            Some(b' ' | b'\t') if len == 0 => return bad(MissReason::Folding),
            Some(b' ' | b'\t') => return bad(MissReason::SpaceBeforeColon),
            Some(_) => return bad(MissReason::Name),
            None => return Err(Stop::Partial),
        }
        if len > MAX_NAME_LEN {
            return bad(MissReason::NameTooLong);
        }
        let mut value_start = name_end + 1;
        while matches!(buf.get(value_start), Some(b' ' | b'\t')) {
            value_start += 1;
        }
        let end = value_end(buf, value_start)?;
        let mut value_stop = end;
        while value_stop > value_start && matches!(buf[value_stop - 1], b' ' | b'\t') {
            value_stop -= 1;
        }

        let index = usize::from(block.count);
        let name = &line[..len];
        let class = classify(name);
        if class == Class::Other {
            let key = len ^ usize::from(name[0] | 0x20) ^ (usize::from(name[len - 1] | 0x20) << 2);
            let bucket = 1u64 << (key & 63);
            if other_buckets & bucket != 0 && repeats_other(buf, &headers[..], index, name) {
                block.duplicate_names = true;
            }
            other_buckets |= bucket;
        } else if block.seen & class.bit() != 0 {
            block.dup |= class.bit();
        } else {
            block.seen |= class.bit();
            block.first[class as usize] = index as u8;
        }
        let tail_clean =
            value_start == name_end + 2 && value_stop == end && buf[name_end + 1] == b' ';
        let name_clean = if TITLED & class.bit() == 0 {
            bits & UPPER == 0
        } else {
            bits & UPPER != 0 && class.titled().is_some_and(|titled| name == titled)
        };
        let slot = headers
            .get_mut(index)
            .ok_or(Stop::Bad(MissReason::TooManyHeaders))?;
        *slot = Header {
            name_start: i as u32,
            value_start: value_start as u32,
            value_len: (value_stop - value_start) as u32,
            name_len: len as u8,
            class,
            flags: ((bits >> 1) & (NAME_HAS_UPPER | NAME_HAS_UNDERSCORE))
                | (u8::from(tail_clean && name_clean) * LINE_IS_CLEAN)
                | (u8::from(tail_clean) * TAIL_IS_CLEAN),
        };
        block.bits |= bits;
        block.count += 1;
        i = end + 2;
    }
}
