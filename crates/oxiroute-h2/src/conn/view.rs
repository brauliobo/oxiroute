//! A decoded request head in compact form: one byte arena holding the pseudo-header values and then every regular
//! field (name, value, name, value, ...), plus the end offsets of those fields. The decoder copies each field once,
//! the arena and the offsets come from a small per-thread pool and go back to it when the view is dropped, so a
//! steady stream of requests allocates nothing for their heads.
//!
//! Memory per pending request: the decoded header list bytes (names, values and pseudo-header values, at most
//! `max_header_list_size` less 32 octets per field), 4 octets per name and value, and about 120 octets of record.
use std::cell::RefCell;

/// Pooled buffers kept per thread, and the largest capacities that are kept.
const POOL_MAX: usize = 64;
const KEEP_ARENA: usize = 16 * 1024;
const KEEP_ENDS: usize = 512;

/// The most regular fields a request may carry: `http::HeaderMap` cannot hold more distinct names reliably, and
/// `into_parts` must not fail.
pub const MAX_FIELDS: usize = 12_288;

#[derive(Default)]
struct Buf {
    arena: Vec<u8>,
    ends: Vec<u32>,
}

thread_local! {
    static POOL: RefCell<Vec<Buf>> = const { RefCell::new(Vec::new()) };
}

impl Buf {
    fn take() -> Self {
        POOL.with(|p| p.borrow_mut().pop()).unwrap_or_default()
    }

    fn give(mut self) {
        if self.arena.capacity() > KEEP_ARENA || self.ends.capacity() > KEEP_ENDS {
            return;
        }
        self.arena.clear();
        self.ends.clear();
        // A view dropped while the thread exits finds the pool gone: nothing to return to.
        let _ = POOL.try_with(|p| {
            let mut p = p.borrow_mut();
            if p.len() < POOL_MAX {
                p.push(self);
            }
        });
    }
}

/// The pseudo-headers a request can carry.
#[derive(Clone, Copy)]
pub(super) enum Pseudo {
    Method,
    Scheme,
    Authority,
    Path,
}

type Span = (u32, u32);

/// A validated request head borrowed from its arena; see the [`server`](crate::server) module for the accessors'
/// guarantees.
pub struct RequestView {
    buf: Buf,
    pseudo: [Span; 4],
    seen: u8,
    /// Where the first regular field starts in the arena.
    fields_at: u32,
    pub(super) content_length: u64,
    end_stream: bool,
}

impl RequestView {
    pub(super) fn new(end_stream: bool) -> Self {
        Self {
            buf: Buf::take(),
            pseudo: [(0, 0); 4],
            seen: 0,
            fields_at: 0,
            content_length: super::validate::UNDECLARED,
            end_stream,
        }
    }

    pub(super) fn has(&self, which: Pseudo) -> bool {
        self.seen & (1 << which as u8) != 0
    }

    /// Records a pseudo-header value; they all precede the regular fields.
    pub(super) fn put(&mut self, which: Pseudo, value: &[u8]) {
        let start = self.buf.arena.len() as u32;
        self.buf.arena.extend_from_slice(value);
        self.pseudo[which as usize] = (start, self.buf.arena.len() as u32);
        self.seen |= 1 << which as u8;
    }

    pub(super) fn push(&mut self, name: &[u8], value: &[u8]) {
        if self.buf.ends.is_empty() {
            self.fields_at = self.buf.arena.len() as u32;
        }
        for part in [name, value] {
            self.buf.arena.extend_from_slice(part);
            self.buf.ends.push(self.buf.arena.len() as u32);
        }
    }

    pub(super) fn len(&self) -> usize {
        self.buf.ends.len() / 2
    }

    fn pseudo(&self, which: Pseudo) -> &[u8] {
        let (start, end) = self.pseudo[which as usize];
        &self.buf.arena[start as usize..end as usize]
    }

    pub(super) fn optional(&self, which: Pseudo) -> Option<&[u8]> {
        self.has(which).then(|| self.pseudo(which))
    }

    /// The `:method` value.
    #[must_use]
    pub fn method(&self) -> &[u8] {
        self.pseudo(Pseudo::Method)
    }

    /// The `:scheme` value; absent for `CONNECT`.
    #[must_use]
    pub fn scheme(&self) -> Option<&[u8]> {
        self.optional(Pseudo::Scheme)
    }

    #[must_use]
    pub fn authority(&self) -> Option<&[u8]> {
        self.optional(Pseudo::Authority)
    }

    /// The `:path` value as `http::Uri` keeps it: query included, a fragment cut off; empty for `CONNECT`.
    #[must_use]
    pub fn path(&self) -> &[u8] {
        self.pseudo(Pseudo::Path)
    }

    /// The regular (non-pseudo) fields in wire order, names lowercase.
    #[must_use]
    pub fn fields(&self) -> Fields<'_> {
        Fields {
            arena: &self.buf.arena,
            ends: self.buf.ends.chunks_exact(2),
            start: self.fields_at as usize,
        }
    }

    /// The declared `content-length`, validated (digits only, every occurrence equal); `None` when absent.
    #[must_use]
    pub fn content_length(&self) -> Option<u64> {
        (self.content_length != super::validate::UNDECLARED).then_some(self.content_length)
    }

    /// The `HEADERS` frame carried `END_STREAM`: the request has no body.
    #[must_use]
    pub fn is_end_stream(&self) -> bool {
        self.end_stream
    }
}

impl Drop for RequestView {
    fn drop(&mut self) {
        std::mem::take(&mut self.buf).give();
    }
}

/// Iterator over the regular fields of a [`RequestView`].
#[derive(Clone)]
pub struct Fields<'a> {
    arena: &'a [u8],
    ends: std::slice::ChunksExact<'a, u32>,
    start: usize,
}

impl<'a> Iterator for Fields<'a> {
    type Item = (&'a [u8], &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let ends = self.ends.next()?;
        let (name_end, value_end) = (ends[0] as usize, ends[1] as usize);
        let field = (
            &self.arena[self.start..name_end],
            &self.arena[name_end..value_end],
        );
        self.start = value_end;
        Some(field)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.ends.size_hint()
    }
}

impl ExactSizeIterator for Fields<'_> {}
