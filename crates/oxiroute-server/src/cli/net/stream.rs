use std::io::{self, BufRead, BufReader, Read};

use super::{
    endpoint::Stream,
    response::{BodyFraming, ParsedHead, Response, read_http_head},
};
use crate::cli::error::CliError;

const MAX_CHUNK_LINE_BYTES: u64 = 4 * 1024;

enum Remaining {
    Done,
    Length(usize),
    Chunk(usize),
    ChunkHead,
    Close,
}

/// The decoded body of a successful answer, read as it arrives instead of buffered whole.
pub(crate) struct BodyStream {
    reader: BufReader<Stream>,
    remaining: Remaining,
}

impl BodyStream {
    pub(super) fn open(mut reader: BufReader<Stream>) -> Result<Self, CliError> {
        loop {
            let head = ParsedHead::parse(&read_http_head(&mut reader)?)?;
            if (100..200).contains(&head.status) && head.status != 101 {
                continue;
            }
            if !(200..300).contains(&head.status) {
                return Err(Response::complete(head, &mut reader)?.failure());
            }
            let remaining = match head.framing {
                BodyFraming::None => Remaining::Done,
                BodyFraming::Length(length) => Remaining::Length(length),
                BodyFraming::Chunked => Remaining::ChunkHead,
                BodyFraming::Close => Remaining::Close,
            };
            return Ok(Self { reader, remaining });
        }
    }

    fn chunk_head(&mut self) -> io::Result<usize> {
        let mut line = Vec::new();
        self.reader
            .by_ref()
            .take(MAX_CHUNK_LINE_BYTES)
            .read_until(b'\n', &mut line)?;
        let size = line
            .strip_suffix(b"\r\n")
            .and_then(|line| line.split(|byte| *byte == b';').next())
            .and_then(|digits| std::str::from_utf8(digits).ok())
            .and_then(|digits| usize::from_str_radix(digits.trim(), 16).ok());
        size.ok_or_else(|| invalid("invalid HTTP chunk size"))
    }

    fn chunk_end(&mut self) -> io::Result<()> {
        let mut terminator = [0; 2];
        self.reader.read_exact(&mut terminator)?;
        if terminator == *b"\r\n" {
            Ok(())
        } else {
            Err(invalid("invalid HTTP chunk terminator"))
        }
    }

    fn take(&mut self, buffer: &mut [u8], left: usize) -> io::Result<usize> {
        let wanted = buffer.len().min(left);
        match self.reader.read(&mut buffer[..wanted])? {
            0 => Err(io::ErrorKind::UnexpectedEof.into()),
            read => Ok(read),
        }
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

impl Read for BodyStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        loop {
            match self.remaining {
                Remaining::Done => return Ok(0),
                Remaining::Close => return self.reader.read(buffer),
                Remaining::Length(0) => self.remaining = Remaining::Done,
                Remaining::Length(left) => {
                    let read = self.take(buffer, left)?;
                    self.remaining = Remaining::Length(left - read);
                    return Ok(read);
                }
                Remaining::ChunkHead => {
                    self.remaining = match self.chunk_head()? {
                        0 => {
                            self.chunk_end()?;
                            Remaining::Done
                        }
                        size => Remaining::Chunk(size),
                    };
                }
                Remaining::Chunk(0) => {
                    self.chunk_end()?;
                    self.remaining = Remaining::ChunkHead;
                }
                Remaining::Chunk(left) => {
                    let read = self.take(buffer, left)?;
                    self.remaining = Remaining::Chunk(left - read);
                    return Ok(read);
                }
            }
        }
    }
}

impl BodyStream {
    pub(crate) fn lines(self) -> impl Iterator<Item = io::Result<String>> {
        BufReader::new(self).lines()
    }
}

#[cfg(test)]
mod tests;
