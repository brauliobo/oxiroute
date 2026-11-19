use serde::{Deserialize, Deserializer, de::Error as _};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Count(pub(crate) u64);

#[derive(Deserialize)]
#[serde(untagged)]
enum Repr {
    Number(u64),
    Text(String),
}

impl<'de> Deserialize<'de> for Count {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match Repr::deserialize(deserializer)? {
            Repr::Number(value) => Ok(Self(value)),
            Repr::Text(text) => text.parse().map(Self).map_err(D::Error::custom),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Label(pub(crate) String);

#[derive(Deserialize)]
#[serde(untagged)]
enum LabelRepr {
    Text(String),
    Number(u64),
    List(Vec<String>),
}

impl<'de> Deserialize<'de> for Label {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self(match LabelRepr::deserialize(deserializer)? {
            LabelRepr::Text(text) => text,
            LabelRepr::Number(number) => number.to_string(),
            LabelRepr::List(items) => items.join(","),
        }))
    }
}
