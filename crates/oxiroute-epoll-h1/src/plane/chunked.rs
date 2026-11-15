pub(crate) const HEADER_ROOM: usize = 18;
pub(crate) const LAST_CHUNK: &[u8] = b"0\r\n\r\n";
const MAX_SIZE_DIGITS: u8 = 15;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Malformed;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Step {
    Pending,
    Done(usize),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Size { value: u64, digits: u8 },
    Extension(u64),
    SizeLf(u64),
    Data(u64),
    DataCr,
    DataLf,
    TrailerStart,
    TrailerLine,
    TrailerEndLf,
    Done,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ChunkScanner {
    state: State,
}

impl ChunkScanner {
    pub(crate) const fn new() -> Self {
        Self {
            state: State::Size {
                value: 0,
                digits: 0,
            },
        }
    }

    pub(crate) fn feed(&mut self, data: &[u8]) -> Result<Step, Malformed> {
        let mut at = 0;
        while at < data.len() {
            if self.state == State::Done {
                return Ok(Step::Done(at));
            }
            at += self.consume(&data[at..])?;
        }
        Ok(if self.state == State::Done {
            Step::Done(at)
        } else {
            Step::Pending
        })
    }

    fn consume(&mut self, data: &[u8]) -> Result<usize, Malformed> {
        if let State::Data(left) = self.state {
            let take = usize::try_from(left).map_or(data.len(), |left| left.min(data.len()));
            let remaining = left - take as u64;
            self.state = if remaining == 0 {
                State::DataCr
            } else {
                State::Data(remaining)
            };
            return Ok(take);
        }
        self.state = self.next_state(data[0])?;
        Ok(1)
    }

    fn next_state(self, byte: u8) -> Result<State, Malformed> {
        match (self.state, byte) {
            (State::Size { value, digits }, _) if byte.is_ascii_hexdigit() => {
                let digit = u64::from((byte as char).to_digit(16).unwrap_or_default());
                if digits >= MAX_SIZE_DIGITS {
                    return Err(Malformed);
                }
                Ok(State::Size {
                    value: value << 4 | digit,
                    digits: digits + 1,
                })
            }
            (State::Size { value, digits }, b';') if digits > 0 => Ok(State::Extension(value)),
            (State::Size { value, digits }, b'\r') if digits > 0 => Ok(State::SizeLf(value)),
            (State::Extension(value), b'\r') => Ok(State::SizeLf(value)),
            (State::Extension(_), b'\n') => Err(Malformed),
            (State::Extension(value), _) => Ok(State::Extension(value)),
            (State::SizeLf(0) | State::TrailerStart | State::TrailerLine, b'\n') => {
                Ok(State::TrailerStart)
            }
            (State::SizeLf(value), b'\n') => Ok(State::Data(value)),
            (State::DataCr, b'\r') => Ok(State::DataLf),
            (State::DataLf, b'\n') => Ok(State::Size {
                value: 0,
                digits: 0,
            }),
            (State::TrailerStart, b'\r') => Ok(State::TrailerEndLf),
            (State::TrailerStart | State::TrailerLine, _) => Ok(State::TrailerLine),
            (State::TrailerEndLf, b'\n') => Ok(State::Done),
            _ => Err(Malformed),
        }
    }
}

pub(crate) fn write_chunk_header(buf: &mut [u8], end: usize, len: usize) -> usize {
    buf[end - 2..end].copy_from_slice(b"\r\n");
    let mut start = end - 2;
    let mut rest = len;
    loop {
        start -= 1;
        buf[start] = b"0123456789abcdef"[rest & 15];
        rest >>= 4;
        if rest == 0 {
            return start;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &[u8] =
        b"5\r\nhello\r\n10;ext=1\r\n0123456789abcdef\r\n0\r\nTrailer: x\r\nA: b\r\n\r\nNEXT";

    fn scan_split(parts: &[&[u8]]) -> Result<Step, Malformed> {
        let mut scanner = ChunkScanner::new();
        let mut consumed = 0;
        for part in parts {
            match scanner.feed(part)? {
                Step::Done(at) => return Ok(Step::Done(consumed + at)),
                Step::Pending => consumed += part.len(),
            }
        }
        Ok(Step::Pending)
    }

    #[test]
    fn a_body_ends_where_its_last_chunk_and_trailers_end() {
        let end = BODY.len() - 4;
        assert_eq!(scan_split(&[BODY]), Ok(Step::Done(end)));
    }

    #[test]
    fn the_end_is_the_same_wherever_the_bytes_are_split() {
        let end = BODY.len() - 4;
        for first in 0..BODY.len() {
            for second in first..BODY.len() {
                let parts = [&BODY[..first], &BODY[first..second], &BODY[second..]];
                assert_eq!(scan_split(&parts), Ok(Step::Done(end)), "{first} {second}");
            }
        }
    }

    #[test]
    fn a_body_without_trailers_ends_after_the_blank_line() {
        assert_eq!(scan_split(&[b"3\r\nabc\r\n0\r\n\r\nX"]), Ok(Step::Done(13)));
        assert_eq!(scan_split(&[b"0\r\n\r\n"]), Ok(Step::Done(5)));
    }

    #[test]
    fn an_incomplete_body_is_pending() {
        assert_eq!(scan_split(&[b"3\r\nab"]), Ok(Step::Pending));
        assert_eq!(scan_split(&[b"3\r\nabc\r\n0\r\n"]), Ok(Step::Pending));
        assert_eq!(scan_split(&[b""]), Ok(Step::Pending));
    }

    #[test]
    fn malformed_framing_is_refused() {
        for bad in [
            &b"x\r\n"[..],
            b"\r\n",
            b"3\nabc\r\n",
            b"3\r\nabcd\r\n",
            b"3\r\nabc\n",
            b"1234567890123456\r\n",
            b"3;a\n",
            b"0\r\n\rX",
        ] {
            assert_eq!(scan_split(&[bad]), Err(Malformed), "{bad:?}");
        }
    }

    #[test]
    fn a_chunk_header_is_written_right_aligned_before_the_data() {
        let mut buf = [b'.'; 32];
        let start = write_chunk_header(&mut buf, HEADER_ROOM, 0x1a2b);
        assert_eq!(&buf[start..HEADER_ROOM], b"1a2b\r\n");
        let start = write_chunk_header(&mut buf, HEADER_ROOM, 0);
        assert_eq!(&buf[start..HEADER_ROOM], b"0\r\n");
    }
}
