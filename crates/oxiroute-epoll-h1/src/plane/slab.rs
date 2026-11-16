use std::os::fd::OwnedFd;

use super::{client::Client, service::Service, upstream::Upstream, wake::Waker};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Token {
    pub(crate) index: u32,
    pub(crate) generation: u32,
}

impl Token {
    pub(crate) const fn to_u64(self) -> u64 {
        (self.generation as u64) << 32 | self.index as u64
    }

    pub(crate) const fn from_u64(raw: u64) -> Self {
        Self {
            index: raw as u32,
            generation: (raw >> 32) as u32,
        }
    }
}

pub(crate) enum Body<S: Service> {
    Free,
    Busy,
    Listener { fd: OwnedFd, index: usize },
    Wake(Waker),
    Client(Box<Client<S>>),
    Upstream(Upstream<S>),
}

struct Slot<S: Service> {
    generation: u32,
    body: Body<S>,
}

const INITIAL_SLOTS: usize = 1024;

pub(crate) struct Slab<S: Service> {
    slots: Vec<Slot<S>>,
    free: Vec<u32>,
}

impl<S: Service> Slab<S> {
    pub(crate) fn new() -> Self {
        Self {
            slots: Vec::with_capacity(INITIAL_SLOTS),
            free: Vec::with_capacity(INITIAL_SLOTS),
        }
    }

    pub(crate) fn insert(&mut self, body: Body<S>) -> Token {
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            slot.body = body;
            return Token {
                index,
                generation: slot.generation,
            };
        }
        let index = u32::try_from(self.slots.len()).expect("a slab holds fewer than 2^32 slots");
        self.slots.push(Slot {
            generation: 0,
            body,
        });
        Token {
            index,
            generation: 0,
        }
    }

    pub(crate) fn remove(&mut self, token: Token) -> Body<S> {
        let slot = &mut self.slots[token.index as usize];
        slot.generation = slot.generation.wrapping_add(1);
        self.free.push(token.index);
        std::mem::replace(&mut slot.body, Body::Free)
    }

    pub(crate) fn live(&self, index: u32) -> Option<Token> {
        let slot = self.slots.get(index as usize)?;
        (!matches!(slot.body, Body::Free)).then_some(Token {
            index,
            generation: slot.generation,
        })
    }

    pub(crate) fn tokens(&self) -> usize {
        self.slots.len()
    }

    pub(crate) fn token_at(&self, position: usize) -> Option<Token> {
        let index = u32::try_from(position).ok()?;
        self.live(index)
    }

    fn slot(&self, token: Token) -> Option<&Slot<S>> {
        self.slots
            .get(token.index as usize)
            .filter(|slot| slot.generation == token.generation)
    }

    fn slot_mut(&mut self, token: Token) -> Option<&mut Slot<S>> {
        self.slots
            .get_mut(token.index as usize)
            .filter(|slot| slot.generation == token.generation)
    }

    pub(crate) fn body(&self, token: Token) -> Option<&Body<S>> {
        self.slot(token).map(|slot| &slot.body)
    }

    pub(crate) fn take_client(&mut self, token: Token) -> Option<Box<Client<S>>> {
        let slot = self.slot_mut(token)?;
        match std::mem::replace(&mut slot.body, Body::Busy) {
            Body::Client(client) => Some(client),
            other => {
                slot.body = other;
                None
            }
        }
    }

    pub(crate) fn put_client(&mut self, token: Token, client: Box<Client<S>>) {
        self.slots[token.index as usize].body = Body::Client(client);
    }

    pub(crate) fn upstream(&self, token: Token) -> Option<&Upstream<S>> {
        match self.body(token)? {
            Body::Upstream(upstream) => Some(upstream),
            _ => None,
        }
    }

    pub(crate) fn upstream_mut(&mut self, token: Token) -> Option<&mut Upstream<S>> {
        match &mut self.slot_mut(token)?.body {
            Body::Upstream(upstream) => Some(upstream),
            _ => None,
        }
    }
}
