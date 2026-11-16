const HEADER_LEN: usize = 5;

/// Follows the record framing of the ciphertext rustls reads, so the connection knows whether rustls holds half of a
/// record: the kernel can take the read direction over only at a record boundary.
#[derive(Debug, Default)]
pub(crate) struct Records {
    header: [u8; HEADER_LEN],
    header_len: usize,
    body_left: usize,
}

impl Records {
    pub(crate) fn observe(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            bytes = if self.body_left > 0 {
                self.skip_body(bytes)
            } else {
                self.take_header(bytes)
            };
        }
    }

    pub(crate) const fn on_boundary(&self) -> bool {
        self.header_len == 0 && self.body_left == 0
    }

    fn skip_body<'a>(&mut self, bytes: &'a [u8]) -> &'a [u8] {
        let skipped = self.body_left.min(bytes.len());
        self.body_left -= skipped;
        &bytes[skipped..]
    }

    fn take_header<'a>(&mut self, bytes: &'a [u8]) -> &'a [u8] {
        let taken = (HEADER_LEN - self.header_len).min(bytes.len());
        self.header[self.header_len..self.header_len + taken].copy_from_slice(&bytes[..taken]);
        self.header_len += taken;
        if self.header_len == HEADER_LEN {
            self.header_len = 0;
            self.body_left = usize::from(u16::from_be_bytes([self.header[3], self.header[4]]));
        }
        &bytes[taken..]
    }
}

#[cfg(test)]
mod tests {
    use super::{HEADER_LEN, Records};

    fn record(length: usize) -> Vec<u8> {
        let mut bytes = vec![0x17, 0x03, 0x03];
        bytes.extend_from_slice(&u16::try_from(length).unwrap().to_be_bytes());
        bytes.resize(HEADER_LEN + length, 0);
        bytes
    }

    #[test]
    fn a_new_connection_is_on_a_boundary() {
        assert!(Records::default().on_boundary());
    }

    #[test]
    fn whole_records_leave_the_boundary_in_place() {
        let mut records = Records::default();
        records.observe(&[record(0), record(1), record(16_640)].concat());
        assert!(records.on_boundary());
    }

    #[test]
    fn half_a_header_or_half_a_body_is_not_a_boundary() {
        let bytes = record(100);
        for cut in 1..bytes.len() {
            let mut records = Records::default();
            records.observe(&bytes[..cut]);
            assert!(!records.on_boundary(), "cut at {cut}");
            records.observe(&bytes[cut..]);
            assert!(records.on_boundary(), "completed after {cut}");
        }
    }

    #[test]
    fn any_split_of_a_stream_ends_on_the_same_boundary() {
        let bytes = [record(7), record(300), record(0), record(42)].concat();
        for step in 1..64 {
            let mut records = Records::default();
            for chunk in bytes.chunks(step) {
                records.observe(chunk);
            }
            assert!(records.on_boundary(), "step {step}");
        }
    }
}
