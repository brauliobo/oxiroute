use std::{fmt, ops::Deref};

use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{Error as _, IgnoredAny, SeqAccess, Visitor},
};
use thiserror::Error;

/// An error produced when a bounded value exceeds its limit.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum BoundError {
    /// A string exceeds its byte limit.
    #[error("string contains {actual} bytes, exceeding the limit of {maximum}")]
    StringTooLong { actual: usize, maximum: usize },
    /// A vector exceeds its element limit.
    #[error("vector contains {actual} elements, exceeding the limit of {maximum}")]
    VectorTooLong { actual: usize, maximum: usize },
}

/// An owned UTF-8 string whose encoded length is at most `MAX` bytes.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BoundedString<const MAX: usize>(String);

impl<const MAX: usize> BoundedString<MAX> {
    /// Validates `value` before allocating an owned string.
    ///
    /// # Errors
    ///
    /// Returns [`BoundError::StringTooLong`] when the UTF-8 byte length exceeds `MAX`.
    pub fn new(value: &str) -> Result<Self, BoundError> {
        Self::validate(value)?;
        Ok(Self(value.to_owned()))
    }

    /// Returns the contained string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the bounded string and returns its allocation.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }

    fn validate(value: &str) -> Result<(), BoundError> {
        if value.len() > MAX {
            return Err(BoundError::StringTooLong {
                actual: value.len(),
                maximum: MAX,
            });
        }
        Ok(())
    }
}

impl<const MAX: usize> TryFrom<String> for BoundedString<MAX> {
    type Error = BoundError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::validate(&value)?;
        Ok(Self(value))
    }
}

impl<const MAX: usize> Deref for BoundedString<MAX> {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl<const MAX: usize> AsRef<str> for BoundedString<MAX> {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl<const MAX: usize> fmt::Debug for BoundedString<MAX> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl<const MAX: usize> fmt::Display for BoundedString<MAX> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl<const MAX: usize> Serialize for BoundedString<MAX> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de, const MAX: usize> Deserialize<'de> for BoundedString<MAX> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BoundedStringVisitor<const MAX: usize>;

        impl<const MAX: usize> Visitor<'_> for BoundedStringVisitor<MAX> {
            type Value = BoundedString<MAX>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "a UTF-8 string containing at most {MAX} bytes")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                BoundedString::new(value).map_err(E::custom)
            }

            fn visit_borrowed_str<E>(self, value: &'_ str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_str(value)
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                BoundedString::try_from(value).map_err(E::custom)
            }
        }

        deserializer.deserialize_str(BoundedStringVisitor::<MAX>)
    }
}

/// An owned vector containing at most `MAX` elements.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundedVec<T, const MAX: usize>(Vec<T>);

impl<T, const MAX: usize> BoundedVec<T, MAX> {
    /// Creates an empty bounded vector without allocating.
    #[must_use]
    pub const fn new() -> Self {
        Self(Vec::new())
    }

    /// Validates a slice length before cloning its elements.
    ///
    /// # Errors
    ///
    /// Returns [`BoundError::VectorTooLong`] when the slice contains more than `MAX` elements.
    pub fn from_slice(value: &[T]) -> Result<Self, BoundError>
    where
        T: Clone,
    {
        Self::validate_len(value.len())?;
        Ok(Self(value.to_vec()))
    }

    /// Appends one element if capacity remains.
    ///
    /// # Errors
    ///
    /// Returns [`BoundError::VectorTooLong`] without growing the vector when it is full.
    pub fn push(&mut self, value: T) -> Result<(), BoundError> {
        Self::validate_len(self.0.len().saturating_add(1))?;
        self.0.push(value);
        Ok(())
    }

    /// Returns the contained slice.
    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        &self.0
    }

    /// Consumes the bounded vector and returns its allocation.
    #[must_use]
    pub fn into_inner(self) -> Vec<T> {
        self.0
    }

    fn validate_len(len: usize) -> Result<(), BoundError> {
        if len > MAX {
            return Err(BoundError::VectorTooLong {
                actual: len,
                maximum: MAX,
            });
        }
        Ok(())
    }

    pub(crate) fn from_vec_within_bound(value: Vec<T>) -> Self {
        debug_assert!(value.len() <= MAX);
        Self(value)
    }
}

impl<T, const MAX: usize> Default for BoundedVec<T, MAX> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const MAX: usize> TryFrom<Vec<T>> for BoundedVec<T, MAX> {
    type Error = BoundError;

    fn try_from(value: Vec<T>) -> Result<Self, Self::Error> {
        Self::validate_len(value.len())?;
        Ok(Self(value))
    }
}

impl<T, const MAX: usize> Deref for BoundedVec<T, MAX> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T, const MAX: usize> IntoIterator for BoundedVec<T, MAX> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<T: Serialize, const MAX: usize> Serialize for BoundedVec<T, MAX> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de, T: Deserialize<'de>, const MAX: usize> Deserialize<'de> for BoundedVec<T, MAX> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BoundedVecVisitor<T, const MAX: usize>(std::marker::PhantomData<T>);

        impl<'de, T: Deserialize<'de>, const MAX: usize> Visitor<'de> for BoundedVecVisitor<T, MAX> {
            type Value = BoundedVec<T, MAX>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "a sequence containing at most {MAX} elements")
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let size_hint = sequence.size_hint().unwrap_or(0);
                if size_hint > MAX {
                    return Err(A::Error::custom(BoundError::VectorTooLong {
                        actual: size_hint,
                        maximum: MAX,
                    }));
                }

                let mut values = Vec::with_capacity(size_hint);
                while values.len() < MAX {
                    match sequence.next_element()? {
                        Some(value) => values.push(value),
                        None => return Ok(BoundedVec(values)),
                    }
                }
                if sequence.next_element::<IgnoredAny>()?.is_some() {
                    return Err(A::Error::custom(BoundError::VectorTooLong {
                        actual: MAX.saturating_add(1),
                        maximum: MAX,
                    }));
                }
                Ok(BoundedVec(values))
            }
        }

        deserializer.deserialize_seq(BoundedVecVisitor(std::marker::PhantomData))
    }
}
