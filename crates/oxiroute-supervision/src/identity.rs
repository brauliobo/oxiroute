use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{BoundError, BoundedString};

/// Maximum encoded byte length of service and instance identifiers.
pub const MAX_IDENTIFIER_BYTES: usize = 128;

/// An invalid service or instance identifier.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum IdentityError {
    /// The identifier is empty.
    #[error("identifier must not be empty")]
    Empty,
    /// The identifier contains an unsupported character.
    #[error("identifier contains invalid character {character:?} at byte {index}")]
    InvalidCharacter { character: char, index: usize },
    /// The identifier exceeds its encoded bound.
    #[error(transparent)]
    Bound(#[from] BoundError),
}

fn validate_identifier(value: &str) -> Result<(), IdentityError> {
    if value.is_empty() {
        return Err(IdentityError::Empty);
    }
    if let Some((index, character)) = value.char_indices().find(|(_, character)| {
        !character.is_ascii_alphanumeric() && !matches!(character, '-' | '_' | '.')
    }) {
        return Err(IdentityError::InvalidCharacter { character, index });
    }
    Ok(())
}

macro_rules! string_identity {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(BoundedString<MAX_IDENTIFIER_BYTES>);

        impl $name {
            /// Validates the identifier before allocating its owned representation.
            ///
            /// # Errors
            ///
            /// Returns [`IdentityError`] when the value is empty, too long, or contains characters
            /// outside ASCII letters, digits, `.`, `_`, and `-`.
            pub fn new(value: &str) -> Result<Self, IdentityError> {
                validate_identifier(value)?;
                Ok(Self(BoundedString::new(value)?))
            }

            /// Returns the identifier as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = BoundedString::<MAX_IDENTIFIER_BYTES>::deserialize(deserializer)?;
                validate_identifier(value.as_str()).map_err(serde::de::Error::custom)?;
                Ok(Self(value))
            }
        }
    };
}

string_identity!(ServiceId, "A stable logical service identifier.");
string_identity!(InstanceId, "A unique runtime service instance identifier.");

macro_rules! numeric_identity {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(
            Clone,
            Copy,
            Debug,
            Default,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
            Deserialize,
            Serialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub u64);

        impl $name {
            /// Returns the numeric identity value.
            #[must_use]
            pub const fn get(self) -> u64 {
                self.0
            }
        }

        impl From<u64> for $name {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

numeric_identity!(
    GenerationId,
    "A monotonically increasing service generation identifier."
);
numeric_identity!(RequestId, "An RPC request correlation identifier.");
numeric_identity!(Epoch, "A caller-defined monotonic time epoch.");
numeric_identity!(
    Sequence,
    "A monotonically increasing stream sequence number."
);
numeric_identity!(Revision, "A monotonically increasing state revision.");
