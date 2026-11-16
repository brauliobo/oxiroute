use std::{
    fmt, io,
    net::{Ipv4Addr, TcpListener, TcpStream},
    os::fd::AsFd,
};

use nix::sys::socket::{
    setsockopt,
    sockopt::{TcpTlsRx, TcpTlsTx, TcpUlp, TlsCryptoInfo},
};
use rustls::{
    CipherSuite, ConnectionTrafficSecrets, ExtractedSecrets, ProtocolVersion, ServerConnection,
};

const NONCE_LEN: usize = 12;
const GCM_SALT_LEN: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Version {
    Tls12,
    Tls13,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cipher {
    Aes128Gcm,
    Aes256Gcm,
    Chacha20Poly1305,
}

impl Cipher {
    const fn key_len(self) -> usize {
        match self {
            Self::Aes128Gcm => 16,
            Self::Aes256Gcm | Self::Chacha20Poly1305 => 32,
        }
    }
}

/// A protocol version and an AEAD cipher: what the kernel is asked to run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Suite {
    version: Version,
    cipher: Cipher,
}

impl Suite {
    const ALL: [Self; 6] = [
        Self::new(Version::Tls12, Cipher::Aes128Gcm),
        Self::new(Version::Tls12, Cipher::Aes256Gcm),
        Self::new(Version::Tls12, Cipher::Chacha20Poly1305),
        Self::new(Version::Tls13, Cipher::Aes128Gcm),
        Self::new(Version::Tls13, Cipher::Aes256Gcm),
        Self::new(Version::Tls13, Cipher::Chacha20Poly1305),
    ];

    const fn new(version: Version, cipher: Cipher) -> Self {
        Self { version, cipher }
    }

    const fn bit(self) -> u8 {
        1 << (self.version as u8 * 3 + self.cipher as u8)
    }

    const fn kernel_version(self) -> u16 {
        match self.version {
            Version::Tls12 => libc::TLS_1_2_VERSION,
            Version::Tls13 => libc::TLS_1_3_VERSION,
        }
    }

    const fn kernel_cipher(self) -> u16 {
        match self.cipher {
            Cipher::Aes128Gcm => libc::TLS_CIPHER_AES_GCM_128,
            Cipher::Aes256Gcm => libc::TLS_CIPHER_AES_GCM_256,
            Cipher::Chacha20Poly1305 => libc::TLS_CIPHER_CHACHA20_POLY1305,
        }
    }

    /// TLS 1.2 AES-GCM sends an explicit nonce with every record, which the kernel counts up from the value it is given.
    const fn counts_explicit_nonce(self) -> bool {
        matches!(self.version, Version::Tls12) && !matches!(self.cipher, Cipher::Chacha20Poly1305)
    }

    /// The suite a connection negotiated, when it is one the kernel can run.
    pub(crate) fn of(conn: &ServerConnection) -> Option<Self> {
        let version = match conn.protocol_version()? {
            ProtocolVersion::TLSv1_2 => Version::Tls12,
            ProtocolVersion::TLSv1_3 => Version::Tls13,
            _ => return None,
        };
        let cipher = match conn.negotiated_cipher_suite()?.suite() {
            CipherSuite::TLS13_AES_128_GCM_SHA256
            | CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256
            | CipherSuite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256 => Cipher::Aes128Gcm,
            CipherSuite::TLS13_AES_256_GCM_SHA384
            | CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384
            | CipherSuite::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384 => Cipher::Aes256Gcm,
            CipherSuite::TLS13_CHACHA20_POLY1305_SHA256
            | CipherSuite::TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256
            | CipherSuite::TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256 => Cipher::Chacha20Poly1305,
            _ => return None,
        };
        Some(Self::new(version, cipher))
    }
}

/// The suites the running kernel runs for both directions of a connection, as [`probe`] found them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KernelTls {
    suites: u8,
}

impl KernelTls {
    pub(crate) const fn offers(self, suite: Suite) -> bool {
        self.suites & suite.bit() != 0
    }
}

/// Why the kernel cannot run the records of a connection.
#[derive(Debug)]
pub enum KtlsUnavailable {
    /// The probe could not make a loopback connection.
    Probe(io::Error),
    /// The `tls` upper layer protocol is not offered: the module is not loaded (an unprivileged process cannot load it)
    /// or the kernel was built without it.
    UlpMissing(io::Error),
    /// The kernel refused the keys of every suite.
    NoSuite,
}

impl fmt::Display for KtlsUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Probe(error) => write!(formatter, "the kernel probe could not run: {error}"),
            Self::UlpMissing(error) => write!(
                formatter,
                "the kernel does not offer the tls upper layer protocol ({error}): load the tls module on the host"
            ),
            Self::NoSuite => formatter.write_str("the kernel refused the keys of every TLS suite"),
        }
    }
}

impl std::error::Error for KtlsUnavailable {}

/// Asks the kernel on loopback sockets with throw-away keys which suites it runs for the transmit and the receive
/// direction of a connection.
///
/// # Errors
///
/// The `tls` upper layer protocol is missing, or the kernel refuses every suite.
pub fn probe() -> Result<KernelTls, KtlsUnavailable> {
    let mut suites = 0;
    for suite in Suite::ALL {
        if accepts(suite)? {
            suites |= suite.bit();
        }
    }
    if suites == 0 {
        return Err(KtlsUnavailable::NoSuite);
    }
    Ok(KernelTls { suites })
}

fn accepts(suite: Suite) -> Result<bool, KtlsUnavailable> {
    let (socket, _peer) = loopback_pair().map_err(KtlsUnavailable::Probe)?;
    attach(&socket).map_err(KtlsUnavailable::UlpMissing)?;
    let key = vec![1; suite.cipher.key_len()];
    let keyed = crypto_info(suite, &key, [2; NONCE_LEN], 0)
        .and_then(|info| set_keys(&socket, &info, &info).map_err(Into::into));
    Ok(keyed.is_ok())
}

/// An established loopback connection: the `tls` upper layer protocol needs both ends open.
fn loopback_pair() -> io::Result<(TcpStream, TcpStream)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let client = TcpStream::connect(listener.local_addr()?)?;
    Ok((listener.accept()?.0, client))
}

/// Puts the `tls` upper layer protocol on an established TCP socket: it keeps working as plain TCP until keys follow.
pub(crate) fn attach<F: AsFd>(socket: &F) -> io::Result<()> {
    Ok(setsockopt(socket, TcpUlp::default(), b"tls")?)
}

/// Hands both directions of a socket with the `tls` upper layer protocol to the kernel.
pub(crate) fn install<F: AsFd>(
    socket: &F,
    suite: Suite,
    secrets: &ExtractedSecrets,
) -> io::Result<()> {
    let (sent, outgoing) = &secrets.tx;
    let (received, incoming) = &secrets.rx;
    let (key, iv) = traffic(outgoing)?;
    let iv = if suite.counts_explicit_nonce() {
        unused_explicit_nonce(iv, *sent)?
    } else {
        iv
    };
    let tx = crypto_info(suite, key, iv, *sent)?;
    let (key, iv) = traffic(incoming)?;
    let rx = crypto_info(suite, key, iv, *received)?;
    Ok(set_keys(socket, &tx, &rx)?)
}

fn set_keys<F: AsFd>(socket: &F, tx: &TlsCryptoInfo, rx: &TlsCryptoInfo) -> nix::Result<()> {
    setsockopt(socket, TcpTlsTx, tx)?;
    setsockopt(socket, TcpTlsRx, rx)
}

fn traffic(secrets: &ConnectionTrafficSecrets) -> io::Result<(&[u8], [u8; NONCE_LEN])> {
    match secrets {
        ConnectionTrafficSecrets::Aes128Gcm { key, iv }
        | ConnectionTrafficSecrets::Aes256Gcm { key, iv }
        | ConnectionTrafficSecrets::Chacha20Poly1305 { key, iv } => {
            Ok((key.as_ref(), array(iv.as_ref())?))
        }
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "rustls extracted secrets of an unknown cipher",
        )),
    }
}

fn array<const N: usize>(bytes: &[u8]) -> io::Result<[u8; N]> {
    bytes.try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected {N} bytes of key material, found {}", bytes.len()),
        )
    })
}

fn crypto_info(
    suite: Suite,
    key: &[u8],
    iv: [u8; NONCE_LEN],
    sequence: u64,
) -> io::Result<TlsCryptoInfo> {
    let info = libc::tls_crypto_info {
        version: suite.kernel_version(),
        cipher_type: suite.kernel_cipher(),
    };
    let rec_seq = sequence.to_be_bytes();
    let (salt, explicit) = iv.split_at(GCM_SALT_LEN);
    Ok(match suite.cipher {
        Cipher::Aes128Gcm => TlsCryptoInfo::Aes128Gcm(libc::tls12_crypto_info_aes_gcm_128 {
            info,
            iv: array(explicit)?,
            key: array(key)?,
            salt: array(salt)?,
            rec_seq,
        }),
        Cipher::Aes256Gcm => TlsCryptoInfo::Aes256Gcm(libc::tls12_crypto_info_aes_gcm_256 {
            info,
            iv: array(explicit)?,
            key: array(key)?,
            salt: array(salt)?,
            rec_seq,
        }),
        Cipher::Chacha20Poly1305 => {
            TlsCryptoInfo::Chacha20Poly1305(libc::tls12_crypto_info_chacha20_poly1305 {
                info,
                iv,
                key: array(key)?,
                salt: [],
                rec_seq,
            })
        }
    })
}

/// rustls builds the explicit nonce of the record numbered `n` as the start value xor `n`, the kernel counts up from
/// the value it is given. Records `0..sent` used the start value with the low bits of `n` flipped, all inside one
/// aligned block of that many bits, so the counter starts at the next block: no nonce is ever used twice.
fn unused_explicit_nonce(mut iv: [u8; NONCE_LEN], sent: u64) -> io::Result<[u8; NONCE_LEN]> {
    let explicit = &mut iv[GCM_SALT_LEN..];
    let start = u64::from_be_bytes(array(explicit)?);
    let bits = sent.bit_width();
    let block = start.checked_shr(bits).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "a TLS 1.2 connection sent too many records for kernel TLS",
        )
    })?;
    explicit.copy_from_slice(&(block.wrapping_add(1) << bits).to_be_bytes());
    Ok(iv)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counter(iv: [u8; NONCE_LEN]) -> u64 {
        u64::from_be_bytes(iv[GCM_SALT_LEN..].try_into().unwrap())
    }

    #[test]
    fn the_kernel_counter_never_meets_a_nonce_rustls_used() {
        for start in [0, 1, 0x5a5a_5a5a_5a5a_5a5a, u64::MAX, u64::MAX - 7, 1 << 63] {
            for sent in [0u64, 1, 2, 3, 5, 255, 256, 257, 65_535, 1 << 24] {
                let mut iv = [9; NONCE_LEN];
                iv[GCM_SALT_LEN..].copy_from_slice(&start.to_be_bytes());
                let first = counter(unused_explicit_nonce(iv, sent).unwrap());
                let used = |value: u64| (value ^ start) < sent;
                for step in 0..1_000u64 {
                    let value = first.wrapping_add(step);
                    assert!(
                        !used(value),
                        "start {start:#x} sent {sent}: counter {value:#x}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_counter_that_cannot_be_placed_is_an_error() {
        assert!(unused_explicit_nonce([0; NONCE_LEN], u64::MAX).is_err());
    }

    #[test]
    fn the_salt_stays_with_the_key() {
        let mut iv = [7; NONCE_LEN];
        iv[..GCM_SALT_LEN].copy_from_slice(&[1, 2, 3, 4]);
        assert_eq!(
            &unused_explicit_nonce(iv, 3).unwrap()[..GCM_SALT_LEN],
            &[1, 2, 3, 4]
        );
    }

    #[test]
    fn every_suite_has_its_own_bit() {
        let all = Suite::ALL
            .iter()
            .fold(0u8, |bits, suite| bits | suite.bit());
        assert_eq!(all.count_ones(), 6);
    }

    #[test]
    fn the_crypto_info_has_the_kernel_struct_size() {
        for (cipher, size) in [
            (Cipher::Aes128Gcm, 40),
            (Cipher::Aes256Gcm, 56),
            (Cipher::Chacha20Poly1305, 56),
        ] {
            let suite = Suite::new(Version::Tls13, cipher);
            let info = crypto_info(suite, &vec![0; cipher.key_len()], [0; NONCE_LEN], 0).unwrap();
            let length = match info {
                TlsCryptoInfo::Aes128Gcm(info) => std::mem::size_of_val(&info),
                TlsCryptoInfo::Aes256Gcm(info) => std::mem::size_of_val(&info),
                TlsCryptoInfo::Chacha20Poly1305(info) => std::mem::size_of_val(&info),
            };
            assert_eq!(length, size, "{cipher:?}");
        }
    }

    #[test]
    fn the_probe_agrees_with_the_kernel_about_the_tls_ulp() {
        let listed = std::fs::read_to_string("/proc/sys/net/ipv4/tcp_available_ulp")
            .is_ok_and(|ulps| ulps.split_whitespace().any(|ulp| ulp == "tls"));
        match probe() {
            Ok(kernel) => assert!(listed && kernel.suites != 0),
            Err(KtlsUnavailable::UlpMissing(_)) => assert!(!listed),
            Err(other) => panic!("unexpected probe failure: {other}"),
        }
    }
}
