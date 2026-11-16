use std::os::fd::OwnedFd;

use crate::{addr::Address, event::Event};

/// A socket the ring owns. A handle is valid until the socket is closed or released; a stale handle is ignored.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Handle {
    pub(crate) index: u32,
    pub(crate) generation: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
    Recv,
    Send,
    Accept,
    Connect,
    Poll,
}

const GENERATION: u32 = 0x00FF_FFFF;

/// The user data every operation carries, so a completion finds its socket and kind without a lookup table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Meta {
    pub(crate) kind: Kind,
    pub(crate) sub: u8,
    pub(crate) handle: Handle,
}

impl Meta {
    pub(crate) fn pack(self) -> u64 {
        let kind = self.kind as u64;
        kind << 60
            | u64::from(self.sub & 0xF) << 56
            | u64::from(self.handle.generation & GENERATION) << 32
            | u64::from(self.handle.index)
    }

    pub(crate) fn unpack(raw: u64) -> Self {
        let kind = match raw >> 60 {
            0 => Kind::Recv,
            1 => Kind::Send,
            2 => Kind::Accept,
            3 => Kind::Connect,
            _ => Kind::Poll,
        };
        Self {
            kind,
            sub: ((raw >> 56) & 0xF) as u8,
            handle: Handle {
                index: raw as u32,
                generation: ((raw >> 32) as u32) & GENERATION,
            },
        }
    }
}

#[derive(Debug)]
pub(crate) enum Reader {
    Idle,
    Armed(u64),
    Starved,
    Parked {
        buffer: u16,
        taken: usize,
        len: usize,
    },
    Eof,
    Broken,
}

#[derive(Debug)]
pub(crate) enum Writer {
    Idle,
    Flying { key: u64, from: usize },
    Broken,
}

#[derive(Debug)]
pub(crate) enum Connect {
    Idle,
    Flying(u64),
    Done(bool),
}

pub(crate) struct Acceptor {
    pub(crate) enabled: bool,
    pub(crate) flying: Vec<Option<u64>>,
    pub(crate) spare: Vec<Box<Address>>,
    pub(crate) held: Vec<Event>,
}

pub(crate) struct Slot {
    pub(crate) generation: u32,
    pub(crate) live: bool,
    pub(crate) closing: bool,
    pub(crate) releasing: bool,
    pub(crate) tag: u64,
    pub(crate) fd: Option<OwnedFd>,
    pub(crate) reader: Reader,
    pub(crate) writer: Writer,
    pub(crate) connect: Connect,
    pub(crate) poll: Option<u64>,
    pub(crate) acceptor: Option<Acceptor>,
    pub(crate) inflight: u32,
    pub(crate) want_write: bool,
    pub(crate) shutdown_after_send: bool,
}

impl Slot {
    pub(crate) const fn vacant() -> Self {
        Self {
            generation: 0,
            live: false,
            closing: false,
            releasing: false,
            tag: 0,
            fd: None,
            reader: Reader::Idle,
            writer: Writer::Idle,
            connect: Connect::Idle,
            poll: None,
            acceptor: None,
            inflight: 0,
            want_write: false,
            shutdown_after_send: false,
        }
    }

    pub(crate) fn vacant_after(generation: u32) -> Self {
        Self {
            generation: generation.wrapping_add(1) & GENERATION,
            ..Self::vacant()
        }
    }

    pub(crate) fn open(&mut self, fd: OwnedFd, tag: u64) {
        *self = Self {
            generation: self.generation,
            live: true,
            tag,
            fd: Some(fd),
            ..Self::vacant()
        };
    }

    pub(crate) const fn idle(&self) -> bool {
        self.inflight == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_data_names_the_kind_the_sub_operation_and_the_socket() {
        let handle = Handle {
            index: 0xDEAD_BEEF,
            generation: 0x12_3456,
        };
        for kind in [
            Kind::Recv,
            Kind::Send,
            Kind::Accept,
            Kind::Connect,
            Kind::Poll,
        ] {
            let meta = Meta {
                kind,
                sub: 9,
                handle,
            };
            assert_eq!(Meta::unpack(meta.pack()), meta);
        }
    }

    #[test]
    fn a_generation_wraps_inside_its_field_without_touching_the_kind() {
        let handle = Handle {
            index: 1,
            generation: u32::MAX,
        };
        let meta = Meta::unpack(
            Meta {
                kind: Kind::Poll,
                sub: 0,
                handle,
            }
            .pack(),
        );
        assert_eq!(
            (meta.kind, meta.handle.generation),
            (Kind::Poll, GENERATION)
        );
    }
}
