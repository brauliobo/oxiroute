//! Field sections without a dynamic table (RFC 9204 section 4.5): the server advertises capacity 0 and no blocked
//! streams, so a section decodes only from the static table and literals.
use oxiroute_h2::hpack::shared::{huffman_decode, integer_decode};

use super::static_table::LEN;

/// A malformed section, one that references the dynamic table, or Huffman data that does not decode. Every case is
/// a connection error of type `QPACK_DECOMPRESSION_FAILED`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("QPACK decompression failed: {0}")]
pub struct DecodeError(pub &'static str);

/// A string literal as it is on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Str<'a> {
    pub bytes: &'a [u8],
    pub huffman: bool,
}

impl Str<'_> {
    /// Appends the decoded string to `out`.
    ///
    /// # Errors
    ///
    /// The bytes are not valid Huffman data.
    #[inline]
    pub fn write_to(&self, out: &mut Vec<u8>) -> Result<(), DecodeError> {
        if !self.huffman {
            out.extend_from_slice(self.bytes);
        } else if !huffman_decode(self.bytes, out) {
            return Err(DecodeError("invalid Huffman string"));
        }
        Ok(())
    }
}

/// One field line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Line<'a> {
    /// A static table entry: name and value.
    Indexed(u8),
    /// A static table name with a literal value.
    NameRef {
        index: u8,
        value: Str<'a>,
    },
    Literal {
        name: Str<'a>,
        value: Str<'a>,
    },
}

/// The field lines of one section.
pub struct Section<'a> {
    rest: &'a [u8],
}

/// Validates the section prefix (RFC 9204 section 4.5.1): Required Insert Count 0 and a non-negative Base.
///
/// # Errors
///
/// A Required Insert Count above zero needs a dynamic table entry, and a negative Base cannot exist without one.
pub fn section(block: &[u8]) -> Result<Section<'_>, DecodeError> {
    let [required, rest @ ..] = block else {
        return Err(DecodeError("truncated section prefix"));
    };
    if *required != 0 {
        return Err(DecodeError("section requires the dynamic table"));
    }
    let mut rest = rest;
    if rest
        .first()
        .ok_or(DecodeError("truncated section prefix"))?
        & 0x80
        != 0
    {
        return Err(DecodeError("negative base"));
    }
    integer_decode(&mut rest, 7).ok_or(DecodeError("bad delta base"))?;
    Ok(Section { rest })
}

fn take<'a>(src: &mut &'a [u8], len: usize) -> Result<&'a [u8], DecodeError> {
    let (head, rest) = src
        .split_at_checked(len)
        .ok_or(DecodeError("truncated string"))?;
    *src = rest;
    Ok(head)
}

#[inline]
fn string<'a>(src: &mut &'a [u8]) -> Result<Str<'a>, DecodeError> {
    let huffman = src.first().ok_or(DecodeError("truncated string"))? & 0x80 != 0;
    let len = integer_decode(src, 7).ok_or(DecodeError("bad string length"))?;
    Ok(Str {
        bytes: take(src, len)?,
        huffman,
    })
}

#[inline]
fn static_index(src: &mut &[u8], prefix: u32) -> Result<u8, DecodeError> {
    let index = integer_decode(src, prefix).ok_or(DecodeError("bad index"))?;
    if index >= LEN {
        return Err(DecodeError("static index out of range"));
    }
    Ok(index as u8)
}

impl<'a> Section<'a> {
    /// The next field line, `None` at the end of the section.
    ///
    /// # Errors
    ///
    /// The line is malformed or needs the dynamic table.
    #[inline]
    pub fn next_line(&mut self) -> Result<Option<Line<'a>>, DecodeError> {
        let Some(&first) = self.rest.first() else {
            return Ok(None);
        };
        let src = &mut self.rest;
        let line = if first & 0x80 != 0 {
            if first & 0x40 == 0 {
                return Err(DecodeError("dynamic table reference"));
            }
            Line::Indexed(static_index(src, 6)?)
        } else if first & 0x40 != 0 {
            if first & 0x10 == 0 {
                return Err(DecodeError("dynamic table name reference"));
            }
            let index = static_index(src, 4)?;
            Line::NameRef {
                index,
                value: string(src)?,
            }
        } else if first & 0x20 != 0 {
            let huffman = first & 0x08 != 0;
            let len = integer_decode(src, 3).ok_or(DecodeError("bad name length"))?;
            let name = Str {
                bytes: take(src, len)?,
                huffman,
            };
            Line::Literal {
                name,
                value: string(src)?,
            }
        } else {
            return Err(DecodeError("post-base reference"));
        };
        Ok(Some(line))
    }
}

#[cfg(test)]
mod tests;
