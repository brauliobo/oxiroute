//! Response heads the classic client parser reads and the scanner does not, rewritten to the shape the scanner reads.
//!
//! The classic client parses a response with `httparse` and these options: spaces before the colon of a header and
//! obsolete line folding are accepted. That parser also takes empty lines before the status line, lines that end in a
//! bare LF, a status line without a reason phrase, and a reason phrase with bytes above the ASCII range (which it reads
//! as empty). The rewrite reads exactly that grammar and writes what the classic path then writes: CRLF line ends, one
//! space after the status code, `name: value` lines (the name as sent) with each folded value on one line.

use alloc::vec::Vec;

use crate::miss::ScanResponse;
use crate::response::{ResponseHead, scan_response, scan_response_lenient};
use crate::tables::{NAME, TOKEN};

/// How reading a response head that may be unusual ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Canonical {
    /// More bytes could still make a head.
    Partial,
    /// The classic parser refuses these bytes, or no later bytes could change that.
    Refused,
    /// The head is `raw_len` bytes long and its canonical form was appended to the output.
    Done { raw_len: usize },
}

enum Interrupt {
    Partial,
    Refused,
}

type Step<T> = Result<T, Interrupt>;

struct Cursor<'a> {
    buf: &'a [u8],
    at: usize,
}

const fn is_value_byte(byte: u8) -> bool {
    matches!(byte, b'\t' | b' '..=b'~' | 0x80..=0xff)
}

impl<'a> Cursor<'a> {
    fn peek(&self) -> Step<u8> {
        self.buf.get(self.at).copied().ok_or(Interrupt::Partial)
    }

    fn take(&mut self) -> Step<u8> {
        let byte = self.peek()?;
        self.at += 1;
        Ok(byte)
    }

    fn expect(&mut self, wanted: u8) -> Step<()> {
        if self.take()? == wanted {
            Ok(())
        } else {
            Err(Interrupt::Refused)
        }
    }

    fn newline(&mut self) -> Step<()> {
        match self.take()? {
            b'\n' => Ok(()),
            b'\r' => self.expect(b'\n'),
            _ => Err(Interrupt::Refused),
        }
    }

    fn skip_empty_lines(&mut self) -> Step<()> {
        while matches!(self.peek()?, b'\r' | b'\n') {
            self.newline()?;
        }
        Ok(())
    }

    fn token_run(&mut self) -> Step<&'a [u8]> {
        let start = self.at;
        while let Some(&byte) = self.buf.get(self.at)
            && NAME[usize::from(byte)] & TOKEN != 0
        {
            self.at += 1;
        }
        self.peek()?;
        if self.at == start {
            return Err(Interrupt::Refused);
        }
        Ok(&self.buf[start..self.at])
    }

    fn colon(&mut self) -> Step<()> {
        let mut byte = self.take()?;
        while matches!(byte, b' ' | b'\t') {
            byte = self.take()?;
            if byte == b':' {
                return Ok(());
            }
        }
        if byte == b':' {
            Ok(())
        } else {
            Err(Interrupt::Refused)
        }
    }

    fn reason(&mut self, out: &mut Vec<u8>) -> Step<()> {
        let start = self.at;
        let mut obsolete = false;
        let end = loop {
            match self.take()? {
                b'\n' => break self.at - 1,
                b'\r' => {
                    self.expect(b'\n')?;
                    break self.at - 2;
                }
                byte if is_value_byte(byte) => obsolete |= byte >= 0x80,
                _ => return Err(Interrupt::Refused),
            }
        };
        if !obsolete {
            out.extend_from_slice(&self.buf[start..end]);
        }
        Ok(())
    }

    fn status_line(&mut self, out: &mut Vec<u8>) -> Step<()> {
        self.skip_empty_lines()?;
        for wanted in *b"HTTP/1." {
            self.expect(wanted)?;
        }
        let minor = self.take()?;
        if !matches!(minor, b'0' | b'1') {
            return Err(Interrupt::Refused);
        }
        self.expect(b' ')?;
        let mut code = [0; 3];
        for digit in &mut code {
            *digit = self.take()?;
            if !digit.is_ascii_digit() {
                return Err(Interrupt::Refused);
            }
        }
        out.extend_from_slice(b"HTTP/1.");
        out.push(minor);
        out.push(b' ');
        out.extend_from_slice(&code);
        out.push(b' ');
        match self.take()? {
            b' ' => self.reason(out)?,
            b'\r' => self.expect(b'\n')?,
            b'\n' => {}
            _ => return Err(Interrupt::Refused),
        }
        out.extend_from_slice(b"\r\n");
        Ok(())
    }

    fn after_colon(&mut self) -> Step<&'a [u8]> {
        let start = loop {
            let byte = self.take()?;
            if matches!(byte, b' ' | b'\t') {
                continue;
            }
            if is_value_byte(byte) {
                break self.at - 1;
            }
            self.at -= 1;
            self.newline()?;
            if !matches!(self.peek()?, b' ' | b'\t') {
                return Ok(&self.buf[self.at..self.at]);
            }
        };
        loop {
            while self.peek().is_ok_and(is_value_byte) {
                self.at += 1;
            }
            self.newline()?;
            if !matches!(self.peek()?, b' ' | b'\t') {
                return Ok(&self.buf[start..self.at]);
            }
        }
    }

    fn field(&mut self, out: &mut Vec<u8>) -> Step<()> {
        let name = self.token_run()?;
        self.colon()?;
        let value = self.after_colon()?;
        out.extend_from_slice(name);
        out.extend_from_slice(b": ");
        unfold(value.trim_ascii_end(), out);
        out.extend_from_slice(b"\r\n");
        Ok(())
    }

    fn fields(&mut self, out: &mut Vec<u8>) -> Step<()> {
        while !matches!(self.peek()?, b'\r' | b'\n') {
            self.field(out)?;
        }
        self.newline()?;
        out.extend_from_slice(b"\r\n");
        Ok(())
    }
}

/// A folded value as the classic path stores it: every fold, with the whitespace around it, becomes one space.
fn unfold(value: &[u8], out: &mut Vec<u8>) {
    let mut lines = value.split(|&byte| byte == b'\n');
    let start = out.len();
    out.extend_from_slice(lines.next().unwrap_or_default().trim_ascii_end());
    for line in lines
        .map(<[u8]>::trim_ascii)
        .filter(|line| !line.is_empty())
    {
        if out.len() > start {
            out.push(b' ');
        }
        out.extend_from_slice(line);
    }
}

/// Appends to `out` the head at the start of `buf` as the classic client parser reads it, in the shape the scanner
/// reads: the same status, reason, header names, and values, so a head that is scanned after this is the head the
/// classic path scans. Nothing is appended when the answer is not [`Canonical::Done`].
pub fn canonicalize_response(buf: &[u8], out: &mut Vec<u8>) -> Canonical {
    let start = out.len();
    let mut cursor = Cursor { buf, at: 0 };
    let read = cursor.status_line(out).and_then(|()| cursor.fields(out));
    match read {
        Ok(()) => Canonical::Done { raw_len: cursor.at },
        Err(interrupt) => {
            out.truncate(start);
            match interrupt {
                Interrupt::Partial => Canonical::Partial,
                Interrupt::Refused => Canonical::Refused,
            }
        }
    }
}

/// What [`read_response_head`] found at the start of a buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeadRead {
    /// More bytes could still make a head.
    Partial,
    /// The classic client refuses these bytes, or the head is one this crate does not carry (too many headers, an
    /// option of `Connection` that cannot be removed with the others).
    Refused,
    /// A head of `raw_len` bytes in the buffer. When `canonical`, the offsets of the [`ResponseHead`] refer to the
    /// canonical form that was written to the scratch vector, not to the buffer.
    Head { canonical: bool, raw_len: usize },
}

/// Reads the response head at the start of `raw` the way the classic client parser does: the clean shape through
/// [`scan_response`], anything else through [`canonicalize_response`] into `canonical` (which is cleared first) and
/// [`scan_response_lenient`].
#[inline]
pub fn read_response_head(
    raw: &[u8],
    head: &mut ResponseHead,
    canonical: &mut Vec<u8>,
) -> HeadRead {
    match scan_response(raw, head) {
        ScanResponse::Head => HeadRead::Head {
            canonical: false,
            raw_len: head.head_len as usize,
        },
        ScanResponse::Partial => HeadRead::Partial,
        ScanResponse::Slow => read_unusual_head(raw, head, canonical),
    }
}

#[inline(never)]
fn read_unusual_head(raw: &[u8], head: &mut ResponseHead, canonical: &mut Vec<u8>) -> HeadRead {
    canonical.clear();
    match canonicalize_response(raw, canonical) {
        Canonical::Partial => HeadRead::Partial,
        Canonical::Refused => HeadRead::Refused,
        Canonical::Done { raw_len } => match scan_response_lenient(canonical, head) {
            ScanResponse::Head => HeadRead::Head {
                canonical: true,
                raw_len,
            },
            ScanResponse::Partial | ScanResponse::Slow => HeadRead::Refused,
        },
    }
}
