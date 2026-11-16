use std::{
    fmt, io,
    sync::{Arc, Mutex, PoisonError},
};

use rustls::{
    ExtractedSecrets, KeyLog, ServerConfig, Tls13CipherSuite,
    crypto::tls13::{Hkdf, HkdfExpander, OkmBlock},
};

use super::{NONCE_LEN, traffic};

const CLIENT_TRAFFIC_SECRET: &str = "CLIENT_TRAFFIC_SECRET_0";
const SERVER_TRAFFIC_SECRET: &str = "SERVER_TRAFFIC_SECRET_0";
const LABEL_PREFIX: &[u8] = b"tls13 ";
/// Key updates a connection can have gone through before the kernel took it over. A connection past this is refused.
const MAX_GENERATIONS: usize = 4096;

fn is_traffic_secret(label: &str) -> bool {
    label == CLIENT_TRAFFIC_SECRET || label == SERVER_TRAFFIC_SECRET
}

#[derive(Default)]
struct Initial {
    client: Option<OkmBlock>,
    server: Option<OkmBlock>,
}

/// Keeps the first application traffic secrets of a TLS 1.3 connection: rustls hands the kernel only the key and
/// the IV of the current generation, and the next generations derive from the secret.
pub(crate) struct Capture {
    inner: Arc<dyn KeyLog>,
    initial: Mutex<Initial>,
}

impl fmt::Debug for Capture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Capture").finish_non_exhaustive()
    }
}

impl KeyLog for Capture {
    fn log(&self, label: &str, client_random: &[u8], secret: &[u8]) {
        let mut initial = self.initial.lock().unwrap_or_else(PoisonError::into_inner);
        match label {
            CLIENT_TRAFFIC_SECRET => initial.client = Some(OkmBlock::new(secret)),
            SERVER_TRAFFIC_SECRET => initial.server = Some(OkmBlock::new(secret)),
            _ => {}
        }
        if self.inner.will_log(label) {
            self.inner.log(label, client_random, secret);
        }
    }

    fn will_log(&self, label: &str) -> bool {
        is_traffic_secret(label) || self.inner.will_log(label)
    }
}

impl Capture {
    /// A copy of `config` that reports its traffic secrets to the returned capture, next to its own key log.
    pub(crate) fn attach(config: &ServerConfig) -> (Arc<ServerConfig>, Arc<Self>) {
        let capture = Arc::new(Self {
            inner: Arc::clone(&config.key_log),
            initial: Mutex::default(),
        });
        let mut config = config.clone();
        config.key_log = Arc::clone(&capture) as Arc<dyn KeyLog>;
        (Arc::new(config), capture)
    }

    fn take(&self) -> Initial {
        std::mem::take(&mut *self.initial.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// The application traffic secrets of both directions of a TLS 1.3 connection, as far as its keys have rolled.
pub(crate) struct Rotation {
    hkdf: &'static dyn Hkdf,
    key_len: usize,
    received: OkmBlock,
    sent: OkmBlock,
}

impl fmt::Debug for Rotation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Rotation").finish_non_exhaustive()
    }
}

type Keys = (Vec<u8>, [u8; NONCE_LEN]);

impl Rotation {
    /// Finds the generation of the secrets rustls extracted among the ones the connection's first secrets lead to.
    pub(crate) fn locate(
        suite: &'static Tls13CipherSuite,
        capture: &Capture,
        extracted: &ExtractedSecrets,
    ) -> io::Result<Self> {
        let initial = capture.take();
        let hkdf = suite.hkdf_provider;
        let key_len = suite.aead_alg.key_len();
        let (client, server) = initial.client.zip(initial.server).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "the handshake did not report its traffic secrets",
            )
        })?;
        Ok(Self {
            hkdf,
            key_len,
            received: generation(hkdf, key_len, client, &extracted.rx.1)?,
            sent: generation(hkdf, key_len, server, &extracted.tx.1)?,
        })
    }

    /// The keys of the next generation of what the peer sends.
    pub(crate) fn next_received(&mut self) -> Keys {
        advance(self.hkdf, self.key_len, &mut self.received)
    }

    /// The keys of the next generation of what this side sends.
    pub(crate) fn next_sent(&mut self) -> Keys {
        advance(self.hkdf, self.key_len, &mut self.sent)
    }
}

fn generation(
    hkdf: &'static dyn Hkdf,
    key_len: usize,
    mut secret: OkmBlock,
    extracted: &rustls::ConnectionTrafficSecrets,
) -> io::Result<OkmBlock> {
    let (key, iv) = traffic(extracted)?;
    for _ in 0..MAX_GENERATIONS {
        if keys(&*hkdf.expander_for_okm(&secret), key_len) == (key.to_vec(), iv) {
            return Ok(secret);
        }
        secret = next_secret(hkdf, &secret);
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "the keys of the connection are not among the generations of its traffic secret",
    ))
}

fn advance(hkdf: &'static dyn Hkdf, key_len: usize, secret: &mut OkmBlock) -> Keys {
    *secret = next_secret(hkdf, secret);
    keys(&*hkdf.expander_for_okm(secret), key_len)
}

fn next_secret(hkdf: &dyn Hkdf, secret: &OkmBlock) -> OkmBlock {
    let expander = hkdf.expander_for_okm(secret);
    expander.expand_block(&[&label(expander.hash_len(), b"traffic upd")])
}

fn keys(expander: &dyn HkdfExpander, key_len: usize) -> Keys {
    let mut key = vec![0; key_len];
    let mut iv = [0; NONCE_LEN];
    expand(expander, b"key", &mut key);
    expand(expander, b"iv", &mut iv);
    (key, iv)
}

fn expand(expander: &dyn HkdfExpander, name: &[u8], output: &mut [u8]) {
    expander
        .expand_slice(&[&label(output.len(), name)], output)
        .expect("a key is far below the HKDF output limit");
}

/// The `HkdfLabel` of RFC 8446 section 7.1 with an empty context.
fn label(length: usize, name: &[u8]) -> Vec<u8> {
    let length = u16::try_from(length).expect("a key is far below 64 KiB");
    let name_length = u8::try_from(LABEL_PREFIX.len() + name.len()).expect("a label is short");
    let mut info = length.to_be_bytes().to_vec();
    info.push(name_length);
    info.extend_from_slice(LABEL_PREFIX);
    info.extend_from_slice(name);
    info.push(0);
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_label_follows_rfc_8446_section_7_1() {
        assert_eq!(
            label(32, b"traffic upd"),
            [&[0, 32, 17][..], b"tls13 traffic upd", &[0]].concat()
        );
    }

    #[test]
    fn a_secret_that_never_leads_to_the_extracted_keys_is_refused() {
        let provider = rustls::crypto::aws_lc_rs::default_provider();
        let suite = provider
            .cipher_suites
            .iter()
            .find_map(rustls::SupportedCipherSuite::tls13)
            .unwrap();
        let secrets = rustls::ConnectionTrafficSecrets::Aes256Gcm {
            key: rustls::crypto::cipher::AeadKey::from([1; 32]),
            iv: rustls::crypto::cipher::Iv::new([2; NONCE_LEN]),
        };
        let found = generation(suite.hkdf_provider, 32, OkmBlock::new(&[3; 32]), &secrets);
        assert_eq!(found.err().unwrap().kind(), io::ErrorKind::InvalidData);
    }
}
