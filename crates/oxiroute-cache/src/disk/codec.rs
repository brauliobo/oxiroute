use std::time::Duration;

use super::{DiskCacheError, invalid_record};

#[derive(Default)]
pub(super) struct Encoder(pub(super) Vec<u8>);

impl Encoder {
    pub(super) fn u16(&mut self, value: u16) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    pub(super) fn u32(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    pub(super) fn u64(&mut self, value: u64) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    pub(super) fn boolean(&mut self, value: bool) {
        self.0.push(u8::from(value));
    }

    pub(super) fn duration(&mut self, value: Duration) {
        self.u64(value.as_secs());
        self.u32(value.subsec_nanos());
    }

    pub(super) fn optional_duration(&mut self, value: Option<Duration>) {
        self.boolean(value.is_some());
        if let Some(value) = value {
            self.duration(value);
        }
    }

    pub(super) fn u32_len(&mut self, value: usize) -> Result<(), DiskCacheError> {
        self.u32(u32::try_from(value).map_err(|_| DiskCacheError::RecordTooLarge)?);
        Ok(())
    }

    pub(super) fn bytes(&mut self, value: &[u8]) -> Result<(), DiskCacheError> {
        self.u32_len(value.len())?;
        self.0.extend_from_slice(value);
        Ok(())
    }

    pub(super) fn bytes_u64(&mut self, value: &[u8]) -> Result<(), DiskCacheError> {
        self.u64(u64::try_from(value.len()).map_err(|_| DiskCacheError::RecordTooLarge)?);
        self.0.extend_from_slice(value);
        Ok(())
    }

    pub(super) fn optional_bytes(&mut self, value: Option<&[u8]>) -> Result<(), DiskCacheError> {
        self.boolean(value.is_some());
        if let Some(value) = value {
            self.bytes(value)?;
        }
        Ok(())
    }
}

pub(super) struct Decoder<'a> {
    remaining: &'a [u8],
}

impl<'a> Decoder<'a> {
    pub(super) const fn new(remaining: &'a [u8]) -> Self {
        Self { remaining }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], DiskCacheError> {
        if self.remaining.len() < length {
            return Err(invalid_record());
        }
        let (value, remaining) = self.remaining.split_at(length);
        self.remaining = remaining;
        Ok(value)
    }

    pub(super) fn u16(&mut self) -> Result<u16, DiskCacheError> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().map_err(|_| invalid_record())?,
        ))
    }

    fn u32(&mut self) -> Result<u32, DiskCacheError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().map_err(|_| invalid_record())?,
        ))
    }

    pub(super) fn u64(&mut self) -> Result<u64, DiskCacheError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| invalid_record())?,
        ))
    }

    pub(super) fn boolean(&mut self) -> Result<bool, DiskCacheError> {
        match self.take(1)?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid_record()),
        }
    }

    pub(super) fn duration(&mut self) -> Result<Duration, DiskCacheError> {
        let seconds = self.u64()?;
        let nanos = self.u32()?;
        if nanos >= 1_000_000_000 {
            return Err(invalid_record());
        }
        Ok(Duration::new(seconds, nanos))
    }

    pub(super) fn optional_duration(&mut self) -> Result<Option<Duration>, DiskCacheError> {
        self.boolean()?.then(|| self.duration()).transpose()
    }

    pub(super) fn count(&mut self, maximum: usize) -> Result<usize, DiskCacheError> {
        let count = usize::try_from(self.u32()?).map_err(|_| invalid_record())?;
        if count > maximum {
            return Err(invalid_record());
        }
        Ok(count)
    }

    pub(super) fn bytes(&mut self, maximum: usize) -> Result<&'a [u8], DiskCacheError> {
        let length = usize::try_from(self.u32()?).map_err(|_| invalid_record())?;
        if length > maximum {
            return Err(invalid_record());
        }
        self.take(length)
    }

    pub(super) fn bytes_u64(&mut self, maximum: usize) -> Result<&'a [u8], DiskCacheError> {
        let length = usize::try_from(self.u64()?).map_err(|_| invalid_record())?;
        if length > maximum {
            return Err(invalid_record());
        }
        self.take(length)
    }

    pub(super) fn string(&mut self, maximum: usize) -> Result<String, DiskCacheError> {
        std::str::from_utf8(self.bytes(maximum)?)
            .map(str::to_owned)
            .map_err(|_| invalid_record())
    }

    pub(super) fn optional_bytes(
        &mut self,
        maximum: usize,
    ) -> Result<Option<&'a [u8]>, DiskCacheError> {
        if self.boolean()? {
            self.bytes(maximum).map(Some)
        } else {
            Ok(None)
        }
    }

    pub(super) fn optional_string(
        &mut self,
        maximum: usize,
    ) -> Result<Option<String>, DiskCacheError> {
        self.optional_bytes(maximum)?
            .map(|value| {
                std::str::from_utf8(value)
                    .map(str::to_owned)
                    .map_err(|_| invalid_record())
            })
            .transpose()
    }

    pub(super) const fn finished(&self) -> bool {
        self.remaining.is_empty()
    }
}
