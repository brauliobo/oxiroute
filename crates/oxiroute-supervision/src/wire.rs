use std::marker::PhantomData;

/// Maps bounded wire failures into one protocol's public error vocabulary.
pub trait BoundedWireProtocol {
    /// Protocol-specific error returned by bounded reads and writes.
    type Error;

    /// Maps an invalid or truncated payload shape.
    fn invalid() -> Self::Error;

    /// Maps an encoded payload that exceeds its byte bound.
    fn too_large(actual: usize, maximum: usize) -> Self::Error;

    /// Maps a failed bounded allocation.
    fn allocation() -> Self::Error;
}

/// A bounded big-endian writer with protocol-specific errors.
pub struct BoundedWireWriter<P: BoundedWireProtocol> {
    bytes: Vec<u8>,
    maximum: usize,
    protocol: PhantomData<P>,
}

#[allow(clippy::missing_errors_doc)]
impl<P: BoundedWireProtocol> BoundedWireWriter<P> {
    /// Creates an empty writer capped at `maximum` encoded bytes.
    #[must_use]
    pub const fn new(maximum: usize) -> Self {
        Self {
            bytes: Vec::new(),
            maximum,
            protocol: PhantomData,
        }
    }

    /// Writes one byte.
    pub fn u8(&mut self, value: u8) -> Result<(), P::Error> {
        self.bytes(&[value])
    }

    /// Writes one big-endian 16-bit integer.
    pub fn u16(&mut self, value: u16) -> Result<(), P::Error> {
        self.bytes(&value.to_be_bytes())
    }

    /// Writes one big-endian 32-bit integer.
    pub fn u32(&mut self, value: u32) -> Result<(), P::Error> {
        self.bytes(&value.to_be_bytes())
    }

    /// Writes one big-endian 64-bit integer.
    pub fn u64(&mut self, value: u64) -> Result<(), P::Error> {
        self.bytes(&value.to_be_bytes())
    }

    /// Writes a 32-bit byte length followed by the exact bytes.
    pub fn length_prefixed(&mut self, value: &[u8]) -> Result<(), P::Error> {
        let length = u32::try_from(value.len()).map_err(|_| P::invalid())?;
        self.u32(length)?;
        self.bytes(value)
    }

    /// Appends bytes without exceeding the configured bound.
    pub fn bytes(&mut self, value: &[u8]) -> Result<(), P::Error> {
        let next = self
            .bytes
            .len()
            .checked_add(value.len())
            .ok_or_else(|| P::too_large(usize::MAX, self.maximum))?;
        if next > self.maximum {
            return Err(P::too_large(next, self.maximum));
        }
        self.bytes
            .try_reserve_exact(value.len())
            .map_err(|_| P::allocation())?;
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    /// Returns the encoded bytes.
    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

/// A bounded big-endian slice reader with protocol-specific shape errors.
pub struct BoundedWireReader<'a, P: BoundedWireProtocol> {
    bytes: &'a [u8],
    position: usize,
    protocol: PhantomData<P>,
}

#[allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]
impl<'a, P: BoundedWireProtocol> BoundedWireReader<'a, P> {
    /// Creates a reader over one already-bounded payload.
    #[must_use]
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            position: 0,
            protocol: PhantomData,
        }
    }

    /// Reads one byte.
    pub fn u8(&mut self) -> Result<u8, P::Error> {
        Ok(self.bytes(1)?[0])
    }

    /// Reads one big-endian 16-bit integer.
    pub fn u16(&mut self) -> Result<u16, P::Error> {
        Ok(u16::from_be_bytes(
            self.bytes(2)?.try_into().expect("fixed wire slice"),
        ))
    }

    /// Reads one big-endian 32-bit integer.
    pub fn u32(&mut self) -> Result<u32, P::Error> {
        Ok(u32::from_be_bytes(
            self.bytes(4)?.try_into().expect("fixed wire slice"),
        ))
    }

    /// Reads one big-endian 64-bit integer.
    pub fn u64(&mut self) -> Result<u64, P::Error> {
        Ok(u64::from_be_bytes(
            self.bytes(8)?.try_into().expect("fixed wire slice"),
        ))
    }

    /// Reads a 32-bit byte length followed by the exact bytes.
    pub fn length_prefixed(&mut self) -> Result<&'a [u8], P::Error> {
        let length = usize::try_from(self.u32()?).map_err(|_| P::invalid())?;
        self.bytes(length)
    }

    /// Reads an exact byte count without advancing on failure.
    pub fn bytes(&mut self, length: usize) -> Result<&'a [u8], P::Error> {
        let end = self.position.checked_add(length).ok_or_else(P::invalid)?;
        let value = self.bytes.get(self.position..end).ok_or_else(P::invalid)?;
        self.position = end;
        Ok(value)
    }

    /// Rejects trailing bytes.
    pub fn finish(self) -> Result<(), P::Error> {
        (self.position == self.bytes.len())
            .then_some(())
            .ok_or_else(P::invalid)
    }
}
