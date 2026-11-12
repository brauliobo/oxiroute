//! Edits a lane applies to a head, in the vocabulary of the `HeaderMap` calls the classic path makes.

use arrayvec::ArrayVec;

use crate::class::{Class, classify};
use crate::fields::MAX_NAME_LEN;
use crate::tables::{NAME, TOKEN, UPPER, is_header_value};

/// Edits one [`Edits`] list holds.
pub const MAX_EDITS: usize = 32;

/// A header name for an edit: lowercase token bytes, with the spelling the classic writers give it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Name<'a> {
    pub(crate) lower: &'a [u8],
    pub(crate) wire: &'a [u8],
    pub(crate) class: Class,
}

impl<'a> Name<'a> {
    /// A name from lowercase token bytes: `None` when empty, too long, not a token or not lowercase.
    #[must_use]
    pub fn new(lower: &'a [u8]) -> Option<Self> {
        let valid = !lower.is_empty()
            && lower.len() <= MAX_NAME_LEN
            && lower
                .iter()
                .all(|&b| NAME[usize::from(b)] & (TOKEN | UPPER) == TOKEN);
        valid.then(|| {
            let class = classify(lower);
            Self {
                lower,
                wire: class.titled().unwrap_or(lower),
                class,
            }
        })
    }

    /// A name whose class is already known: `class` must be what [`Name::new`] finds for `lower`, which a caller that
    /// built the name once has seen.
    #[must_use]
    pub fn with_class(lower: &'a [u8], class: Class) -> Self {
        debug_assert_eq!(Self::new(lower).map(|name| name.class), Some(class));
        Self {
            lower,
            wire: class.titled().unwrap_or(lower),
            class,
        }
    }

    /// The name of a known class. Not for [`Class::Other`], whose name is not known.
    #[must_use]
    pub const fn known(class: Class) -> Name<'static> {
        let lower = class.lower();
        let wire = match class.titled() {
            Some(titled) => titled,
            None => lower,
        };
        Name { lower, wire, class }
    }

    #[must_use]
    pub const fn class(&self) -> Class {
        self.class
    }

    #[must_use]
    pub const fn lower(&self) -> &'a [u8] {
        self.lower
    }
}

/// Names of the classes the scanner knows, ready for edits.
pub mod names {
    use super::{Class, Name};

    pub const HOST: Name<'static> = Name::known(Class::Host);
    pub const CONNECTION: Name<'static> = Name::known(Class::Connection);
    pub const KEEP_ALIVE: Name<'static> = Name::known(Class::KeepAlive);
    pub const PROXY_CONNECTION: Name<'static> = Name::known(Class::ProxyConnection);
    pub const PROXY_AUTHENTICATE: Name<'static> = Name::known(Class::ProxyAuthenticate);
    pub const PROXY_AUTHORIZATION: Name<'static> = Name::known(Class::ProxyAuthorization);
    pub const TE: Name<'static> = Name::known(Class::Te);
    pub const TRAILER: Name<'static> = Name::known(Class::Trailer);
    pub const TRANSFER_ENCODING: Name<'static> = Name::known(Class::TransferEncoding);
    pub const UPGRADE: Name<'static> = Name::known(Class::Upgrade);
    pub const EXPECT: Name<'static> = Name::known(Class::Expect);
    pub const CONTENT_LENGTH: Name<'static> = Name::known(Class::ContentLength);
    pub const X_REQUEST_ID: Name<'static> = Name::known(Class::XRequestId);
    pub const X_UPSTREAM_REQUEST_ID: Name<'static> = Name::known(Class::XUpstreamRequestId);
    pub const DATE: Name<'static> = Name::known(Class::Date);
    pub const SERVER: Name<'static> = Name::known(Class::Server);
    pub const SET_COOKIE: Name<'static> = Name::known(Class::SetCookie);
    pub const CACHE_CONTROL: Name<'static> = Name::known(Class::CacheControl);
    pub const CONTENT_TYPE: Name<'static> = Name::known(Class::ContentType);
    pub const CONTENT_ENCODING: Name<'static> = Name::known(Class::ContentEncoding);
    pub const ACCEPT_RANGES: Name<'static> = Name::known(Class::AcceptRanges);
    pub const AGE: Name<'static> = Name::known(Class::Age);
}

/// Bytes valid as a `HeaderValue`, written as they are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Value<'a>(pub(crate) &'a [u8]);

impl<'a> Value<'a> {
    /// `None` when a byte is a control byte other than a tab, or DEL.
    #[must_use]
    pub fn new(bytes: &'a [u8]) -> Option<Self> {
        is_header_value(bytes).then_some(Self(bytes))
    }

    /// A value the caller produced and knows to be valid, such as a hex id: not checked.
    /// Bytes that [`is_header_value`] refuses make a malformed head, nothing worse.
    #[must_use]
    pub const fn trusted(bytes: &'a [u8]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &'a [u8] {
        self.0
    }
}

/// One edit, with the `HeaderMap` call it stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit<'a> {
    /// `HeaderMap::remove`: all values of the name; the last entry takes the removed entry's place.
    Remove(Name<'a>),
    /// `HeaderMap::insert`: the first value is replaced in place and the others dropped, or a new entry is appended.
    Set(Name<'a>, Value<'a>),
    /// `HeaderMap::append`: a value after the last of the name, or a new entry at the end.
    Add(Name<'a>, Value<'a>),
}

impl<'a> Edit<'a> {
    pub(crate) const fn name(&self) -> &Name<'a> {
        match self {
            Self::Remove(name) | Self::Set(name, _) | Self::Add(name, _) => name,
        }
    }

    pub(crate) const fn value(&self) -> Option<Value<'a>> {
        match self {
            Self::Remove(_) => None,
            Self::Set(_, value) | Self::Add(_, value) => Some(*value),
        }
    }
}

/// The list holds [`MAX_EDITS`] edits already.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditsFull;

/// Edits applied in order to a scanned head by the writers. Fixed capacity, no allocation.
#[derive(Clone, Debug)]
pub struct Edits<'a> {
    list: ArrayVec<Edit<'a>, MAX_EDITS>,
    bytes: usize,
}

impl Default for Edits<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> Edits<'a> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            list: ArrayVec::new_const(),
            bytes: 0,
        }
    }

    pub fn clear(&mut self) {
        self.list.clear();
        self.bytes = 0;
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.list.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Bytes the added lines take: their names, values and separators.
    #[must_use]
    pub const fn added_bytes(&self) -> usize {
        self.bytes
    }

    #[must_use]
    pub fn as_slice(&self) -> &[Edit<'a>] {
        &self.list
    }

    /// Appends an edit.
    ///
    /// # Errors
    /// [`EditsFull`] when [`MAX_EDITS`] edits are queued.
    pub fn push(&mut self, edit: Edit<'a>) -> Result<(), EditsFull> {
        self.list.try_push(edit).map_err(|_| EditsFull)?;
        self.bytes += edit
            .value()
            .map_or(0, |v| edit.name().wire.len() + v.0.len() + 4);
        Ok(())
    }

    /// # Errors
    /// [`EditsFull`] when [`MAX_EDITS`] edits are queued.
    pub fn remove(&mut self, name: Name<'a>) -> Result<(), EditsFull> {
        self.push(Edit::Remove(name))
    }

    /// # Errors
    /// [`EditsFull`] when [`MAX_EDITS`] edits are queued.
    pub fn set(&mut self, name: Name<'a>, value: Value<'a>) -> Result<(), EditsFull> {
        self.push(Edit::Set(name, value))
    }

    /// # Errors
    /// [`EditsFull`] when [`MAX_EDITS`] edits are queued.
    pub fn add(&mut self, name: Name<'a>, value: Value<'a>) -> Result<(), EditsFull> {
        self.push(Edit::Add(name, value))
    }
}
