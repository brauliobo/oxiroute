use std::fmt;

use serde::{Deserialize, Deserializer, de::Error as _};

pub(crate) fn deserialize_validated<'de, D, W, T, E>(
    deserializer: D,
    validate: impl FnOnce(W) -> Result<T, E>,
) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    W: Deserialize<'de>,
    E: fmt::Display,
{
    validate(W::deserialize(deserializer)?).map_err(D::Error::custom)
}
