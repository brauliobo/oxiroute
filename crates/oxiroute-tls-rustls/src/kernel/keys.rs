use std::{io, os::fd::AsFd};

use nix::sys::socket::{
    setsockopt,
    sockopt::{TcpTlsRx, TcpTlsTx},
};

use super::{Cipher, Suite, Version, crypto_info, secrets::Rotation};

/// The most plaintext one TLS record carries.
pub(crate) const MAX_PLAINTEXT: usize = 16 * 1024;
/// RFC 8446 section 5.5 puts the AES-GCM confidentiality limit at 2^24.5 records under one key; rustls stops at 2^24.
const AES_GCM_RECORD_LIMIT: u64 = 1 << 24;
/// TLS 1.3 rolls its sending key at half of that limit, which leaves room for the records of one `sendfile` call
/// that the count only bounds.
const AES_GCM_ROLL_AFTER: u64 = 1 << 23;

/// The keys the kernel runs for the two directions of a connection, and how many records the sending key has
/// protected: the kernel never rolls them by itself.
#[derive(Debug)]
pub(crate) struct Keys {
    suite: Suite,
    rotation: Option<Rotation>,
    sent: u64,
    limit: Option<u64>,
}

impl Keys {
    pub(crate) fn new(
        suite: Suite,
        rotation: Option<Rotation>,
        sent: u64,
        roll_after: Option<u64>,
    ) -> Self {
        Self {
            suite,
            rotation,
            sent,
            limit: record_limit(suite, roll_after),
        }
    }

    /// Whether the keys of the connection can roll: only TLS 1.3 has a key update.
    pub(crate) const fn rolls(&self) -> bool {
        matches!(self.suite.version, Version::Tls13)
    }

    /// How many plaintext bytes the sending key protects before it has to roll (or, with TLS 1.2, before the
    /// connection ends); `None` when it protects any amount.
    pub(crate) fn bytes_left(&self) -> Option<usize> {
        let records = self.limit?.saturating_sub(self.sent);
        Some(
            usize::try_from(records)
                .map_or(usize::MAX, |records| records.saturating_mul(MAX_PLAINTEXT)),
        )
    }

    /// Whether the sending key has to roll before `bytes` leave: it protects no more, or it protects fewer than
    /// `bytes` and the caller needs them all at once (and rolling early costs nothing).
    pub(crate) fn roll_due(&self, bytes: usize, whole: bool) -> bool {
        match self.bytes_left() {
            Some(0) => true,
            Some(left) => whole && self.rolls() && left < bytes,
            None => false,
        }
    }

    /// Counts what is about to leave: a send of `bytes` makes at most one record per `MAX_PLAINTEXT` of them.
    pub(crate) fn count(&mut self, bytes: usize) {
        self.sent = self
            .sent
            .saturating_add(bytes.div_ceil(MAX_PLAINTEXT) as u64);
    }

    pub(crate) fn roll_sent<F: AsFd>(&mut self, socket: &F) -> io::Result<()> {
        let rotation = self.rotation.as_mut().ok_or_else(unrollable)?;
        let (key, iv) = rotation.next_sent();
        setsockopt(socket, TcpTlsTx, &crypto_info(self.suite, &key, iv, 0)?)?;
        self.sent = 0;
        Ok(())
    }

    pub(crate) fn roll_received<F: AsFd>(&mut self, socket: &F) -> io::Result<()> {
        let rotation = self.rotation.as_mut().ok_or_else(unrollable)?;
        let (key, iv) = rotation.next_received();
        setsockopt(socket, TcpTlsRx, &crypto_info(self.suite, &key, iv, 0)?)?;
        Ok(())
    }
}

fn record_limit(suite: Suite, roll_after: Option<u64>) -> Option<u64> {
    match (suite.version, suite.cipher) {
        (Version::Tls12, Cipher::Chacha20Poly1305) => None,
        (Version::Tls12, _) => Some(AES_GCM_RECORD_LIMIT),
        (Version::Tls13, Cipher::Chacha20Poly1305) => roll_after,
        (Version::Tls13, _) => {
            Some(roll_after.map_or(AES_GCM_ROLL_AFTER, |after| after.min(AES_GCM_ROLL_AFTER)))
        }
    }
}

pub(crate) fn unrollable() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "the keys of a TLS 1.2 connection cannot change: it ends at the AES-GCM record limit and cannot renegotiate",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORD: usize = MAX_PLAINTEXT;

    fn keys(version: Version, cipher: Cipher, sent: u64, roll_after: Option<u64>) -> Keys {
        Keys::new(Suite::new(version, cipher), None, sent, roll_after)
    }

    #[test]
    fn aes_gcm_keys_of_tls_1_3_roll_at_half_of_the_limit_of_the_cipher() {
        let fresh = keys(Version::Tls13, Cipher::Aes128Gcm, 0, None);
        assert_eq!(fresh.bytes_left(), Some((1 << 23) * RECORD));
        let used = keys(Version::Tls13, Cipher::Aes256Gcm, 1 << 22, None);
        assert_eq!(used.bytes_left(), Some((1 << 22) * RECORD));
    }

    #[test]
    fn a_configured_roll_shortens_the_key_but_never_extends_it() {
        let short = keys(Version::Tls13, Cipher::Aes128Gcm, 3, Some(10));
        assert_eq!(short.bytes_left(), Some(7 * RECORD));
        let long = keys(Version::Tls13, Cipher::Aes128Gcm, 0, Some(1 << 40));
        assert_eq!(long.bytes_left(), Some((1 << 23) * RECORD));
    }

    #[test]
    fn chacha_has_no_limit_unless_a_roll_is_configured() {
        let unlimited = keys(Version::Tls13, Cipher::Chacha20Poly1305, 9, None);
        assert_eq!(unlimited.bytes_left(), None);
        let configured = keys(Version::Tls13, Cipher::Chacha20Poly1305, 9, Some(10));
        assert_eq!(configured.bytes_left(), Some(RECORD));
        let tls12 = keys(Version::Tls12, Cipher::Chacha20Poly1305, 9, Some(10));
        assert_eq!(tls12.bytes_left(), None);
    }

    #[test]
    fn tls_1_2_aes_gcm_ends_at_the_limit_because_its_keys_cannot_roll() {
        let mut keys = keys(Version::Tls12, Cipher::Aes128Gcm, 1 << 24, Some(10));
        assert_eq!(keys.bytes_left(), Some(0));
        assert!(keys.roll_due(1, false));
        let socket = std::net::UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let sent = keys.roll_sent(&socket).unwrap_err();
        assert_eq!(sent.kind(), io::ErrorKind::Unsupported);
        let received = keys.roll_received(&socket).unwrap_err();
        assert_eq!(received.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn a_send_counts_one_record_for_each_started_record_of_plaintext() {
        let mut keys = keys(Version::Tls13, Cipher::Aes128Gcm, 0, Some(100));
        for (bytes, records) in [
            (0, 0),
            (1, 1),
            (RECORD, 1),
            (RECORD + 1, 2),
            (3 * RECORD, 3),
        ] {
            let before = keys.bytes_left().unwrap();
            keys.count(bytes);
            let counted = (before - keys.bytes_left().unwrap()) / RECORD;
            assert_eq!(counted, records, "{bytes}");
        }
    }

    #[test]
    fn a_whole_send_rolls_early_and_a_stream_write_only_at_the_limit() {
        let keys = keys(Version::Tls13, Cipher::Aes128Gcm, 8, Some(10));
        assert!(keys.roll_due(5 * RECORD, true));
        assert!(!keys.roll_due(5 * RECORD, false));
        assert!(!keys.roll_due(2 * RECORD, true));
        assert!(!keys.roll_due(0, true));
        let unrollable = Keys::new(Suite::new(Version::Tls12, Cipher::Aes128Gcm), None, 8, None);
        assert!(!unrollable.roll_due(1 << 40, true));
    }
}
