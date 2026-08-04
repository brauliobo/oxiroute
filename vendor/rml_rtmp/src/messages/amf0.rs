use super::deserialization_errors::MessageDeserializationError;
use byteorder::{BigEndian, ReadBytesExt};
use rml_amf0::Amf0Value;
use std::{
    collections::HashMap,
    io::{self, Cursor, Read},
    string::FromUtf8Error,
};

const NUMBER_MARKER: u8 = 0;
const BOOLEAN_MARKER: u8 = 1;
const STRING_MARKER: u8 = 2;
const OBJECT_MARKER: u8 = 3;
const NULL_MARKER: u8 = 5;
const UNDEFINED_MARKER: u8 = 6;
const ECMA_ARRAY_MARKER: u8 = 8;
const OBJECT_END_MARKER: u8 = 9;
const STRICT_ARRAY_MARKER: u8 = 10;

const DEFAULT_MAX_DEPTH: usize = 32;
const DEFAULT_MAX_CONTAINER_ENTRIES: usize = 1_024;
const DEFAULT_MAX_VALUES: usize = 4_096;
const DEFAULT_MAX_STRING_BYTES: usize = u16::MAX as usize;

/// Admission bounds for one decoded AMF0 message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Amf0Limits {
    /// Maximum number of nested object or array containers.
    pub max_depth: usize,
    /// Maximum number of entries in one object or array.
    pub max_container_entries: usize,
    /// Maximum number of scalar and container values in the complete message.
    pub max_values: usize,
    /// Maximum byte length of a normal AMF0 string or object property name.
    pub max_string_bytes: usize,
}

impl Amf0Limits {
    /// Creates explicit admission bounds for one AMF0 message.
    #[must_use]
    pub const fn new(
        max_depth: usize,
        max_container_entries: usize,
        max_values: usize,
        max_string_bytes: usize,
    ) -> Self {
        Self {
            max_depth,
            max_container_entries,
            max_values,
            max_string_bytes,
        }
    }
}

impl Default for Amf0Limits {
    fn default() -> Self {
        Self::new(
            DEFAULT_MAX_DEPTH,
            DEFAULT_MAX_CONTAINER_ENTRIES,
            DEFAULT_MAX_VALUES,
            DEFAULT_MAX_STRING_BYTES,
        )
    }
}

pub(crate) fn deserialize(
    data: &[u8],
    limits: &Amf0Limits,
) -> Result<Vec<Amf0Value>, MessageDeserializationError> {
    Decoder::new(data, limits)
        .decode()
        .map_err(DecodeError::into_message_error)
}

struct Decoder<'a> {
    input: Cursor<&'a [u8]>,
    limits: &'a Amf0Limits,
    values_seen: usize,
}

impl<'a> Decoder<'a> {
    fn new(data: &'a [u8], limits: &'a Amf0Limits) -> Self {
        Self {
            input: Cursor::new(data),
            limits,
            values_seen: 0,
        }
    }

    fn decode(mut self) -> Result<Vec<Amf0Value>, DecodeError> {
        let mut values = Vec::new();
        while let Some(marker) = self.read_optional_marker()? {
            self.reserve_value()?;
            values.push(self.decode_marker(marker, 0)?);
        }
        Ok(values)
    }

    fn decode_required(&mut self, depth: usize) -> Result<Amf0Value, DecodeError> {
        let marker = self.input.read_u8()?;
        self.reserve_value()?;
        self.decode_marker(marker, depth)
    }

    fn decode_marker(&mut self, marker: u8, depth: usize) -> Result<Amf0Value, DecodeError> {
        match marker {
            NUMBER_MARKER => Ok(Amf0Value::Number(self.input.read_f64::<BigEndian>()?)),
            BOOLEAN_MARKER => Ok(Amf0Value::Boolean(self.input.read_u8()? == 1)),
            STRING_MARKER => Ok(Amf0Value::Utf8String(self.read_string()?)),
            OBJECT_MARKER => {
                self.reserve_container(depth)?;
                Ok(Amf0Value::Object(self.read_object(depth + 1)?))
            }
            NULL_MARKER => Ok(Amf0Value::Null),
            UNDEFINED_MARKER => Ok(Amf0Value::Undefined),
            ECMA_ARRAY_MARKER => {
                let declared_entries = self.input.read_u32::<BigEndian>()? as usize;
                self.check_container_entries(declared_entries)?;
                self.reserve_container(depth)?;
                Ok(Amf0Value::Object(self.read_object(depth + 1)?))
            }
            OBJECT_END_MARKER => Err(DecodeError::InvalidFormat),
            STRICT_ARRAY_MARKER => {
                let entries = self.input.read_u32::<BigEndian>()? as usize;
                self.check_container_entries(entries)?;
                self.reserve_container(depth)?;

                let mut values = Vec::with_capacity(entries);
                for _ in 0..entries {
                    values.push(self.decode_required(depth + 1)?);
                }
                Ok(Amf0Value::StrictArray(values))
            }
            marker => Err(DecodeError::UnknownMarker { marker }),
        }
    }

    fn read_object(&mut self, depth: usize) -> Result<HashMap<String, Amf0Value>, DecodeError> {
        let mut properties = HashMap::new();
        let mut entries = 0;

        loop {
            let name_length = self.input.read_u16::<BigEndian>()?;
            if name_length == 0 {
                if self.input.read_u8()? != OBJECT_END_MARKER {
                    return Err(DecodeError::UnexpectedEmptyObjectPropertyName);
                }
                return Ok(properties);
            }

            if entries >= self.limits.max_container_entries {
                return Err(DecodeError::LimitExceeded);
            }
            entries += 1;

            let name = self.read_string_bytes(name_length as usize)?;
            let name = String::from_utf8(name)?;
            let value = self.decode_required(depth)?;
            properties.insert(name, value);
        }
    }

    fn read_string(&mut self) -> Result<String, DecodeError> {
        let length = self.input.read_u16::<BigEndian>()? as usize;
        let bytes = self.read_string_bytes(length)?;
        Ok(String::from_utf8(bytes)?)
    }

    fn read_string_bytes(&mut self, length: usize) -> Result<Vec<u8>, DecodeError> {
        if length > self.limits.max_string_bytes {
            return Err(DecodeError::LimitExceeded);
        }

        let mut bytes = vec![0; length];
        self.input.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    fn read_optional_marker(&mut self) -> Result<Option<u8>, DecodeError> {
        let mut marker = [0; 1];
        if self.input.read(&mut marker)? == 0 {
            Ok(None)
        } else {
            Ok(Some(marker[0]))
        }
    }

    fn reserve_value(&mut self) -> Result<(), DecodeError> {
        if self.values_seen >= self.limits.max_values {
            return Err(DecodeError::LimitExceeded);
        }
        self.values_seen += 1;
        Ok(())
    }

    fn reserve_container(&self, depth: usize) -> Result<(), DecodeError> {
        if depth >= self.limits.max_depth {
            return Err(DecodeError::LimitExceeded);
        }
        Ok(())
    }

    fn check_container_entries(&self, entries: usize) -> Result<(), DecodeError> {
        if entries > self.limits.max_container_entries {
            return Err(DecodeError::LimitExceeded);
        }
        Ok(())
    }
}

#[derive(Debug)]
enum DecodeError {
    InvalidFormat,
    LimitExceeded,
    Io(io::Error),
    UnknownMarker { marker: u8 },
    UnexpectedEmptyObjectPropertyName,
    Utf8(FromUtf8Error),
}

impl DecodeError {
    fn into_message_error(self) -> MessageDeserializationError {
        match self {
            Self::InvalidFormat | Self::LimitExceeded => {
                MessageDeserializationError::InvalidMessageFormat
            }
            Self::Io(error) => MessageDeserializationError::Amf0DeserializationError(
                rml_amf0::Amf0DeserializationError::BufferReadError(error),
            ),
            Self::UnknownMarker { marker } => {
                MessageDeserializationError::Amf0DeserializationError(
                    rml_amf0::Amf0DeserializationError::UnknownMarker { marker },
                )
            }
            Self::UnexpectedEmptyObjectPropertyName => {
                MessageDeserializationError::Amf0DeserializationError(
                    rml_amf0::Amf0DeserializationError::UnexpectedEmptyObjectPropertyName,
                )
            }
            Self::Utf8(error) => MessageDeserializationError::Amf0DeserializationError(
                rml_amf0::Amf0DeserializationError::StringParseError(error),
            ),
        }
    }
}

impl From<io::Error> for DecodeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<FromUtf8Error> for DecodeError {
    fn from(error: FromUtf8Error) -> Self {
        Self::Utf8(error)
    }
}
