use super::RtmpAutoPushError;

pub(super) fn put_string(
    body: &mut Vec<u8>,
    value: &str,
    maximum: usize,
) -> Result<(), RtmpAutoPushError> {
    if value.is_empty() || value.len() > maximum || !value.is_char_boundary(value.len()) {
        return Err(RtmpAutoPushError::TransportUnavailable);
    }
    let length = u16::try_from(value.len()).map_err(|_| RtmpAutoPushError::TransportUnavailable)?;
    body.extend_from_slice(&length.to_be_bytes());
    body.extend_from_slice(value.as_bytes());
    Ok(())
}

pub(super) struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    pub(super) const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    pub(super) fn string(&mut self, maximum: usize) -> Result<String, RtmpAutoPushError> {
        let length = usize::from(self.u16()?);
        if length == 0 || length > maximum {
            return Err(RtmpAutoPushError::TransportUnavailable);
        }
        let bytes = self.bytes(length)?;
        String::from_utf8(bytes).map_err(|_| RtmpAutoPushError::TransportUnavailable)
    }

    pub(super) fn array<const N: usize>(&mut self) -> Result<[u8; N], RtmpAutoPushError> {
        self.bytes(N)?
            .try_into()
            .map_err(|_| RtmpAutoPushError::TransportUnavailable)
    }

    pub(super) fn u8(&mut self) -> Result<u8, RtmpAutoPushError> {
        let byte = *self
            .bytes
            .get(self.position)
            .ok_or(RtmpAutoPushError::TransportUnavailable)?;
        self.position += 1;
        Ok(byte)
    }

    pub(super) fn u16(&mut self) -> Result<u16, RtmpAutoPushError> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    pub(super) fn u32(&mut self) -> Result<u32, RtmpAutoPushError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    pub(super) fn u64(&mut self) -> Result<u64, RtmpAutoPushError> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    pub(super) fn bytes(&mut self, length: usize) -> Result<Vec<u8>, RtmpAutoPushError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(RtmpAutoPushError::TransportUnavailable)?;
        let bytes = self
            .bytes
            .get(self.position..end)
            .ok_or(RtmpAutoPushError::TransportUnavailable)?
            .to_vec();
        self.position = end;
        Ok(bytes)
    }

    pub(super) fn finish(self) -> Result<(), RtmpAutoPushError> {
        (self.position == self.bytes.len())
            .then_some(())
            .ok_or(RtmpAutoPushError::TransportUnavailable)
    }
}
