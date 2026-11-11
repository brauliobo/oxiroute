// Copyright 2026 Cloudflare, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! What the running kernel offers for kernel TLS (kTLS) transmit offload.
//!
//! The kernel answers the questions that decide whether a connection can be converted with defined errors: the `tls`
//! upper layer protocol is missing (module not loaded, kernel without `CONFIG_TLS`), the cipher is refused, or a second
//! key for the same socket is refused (TLS 1.3 `KeyUpdate` needs kernel 6.5). [`probe`] asks it on loopback sockets
//! with throw-away keys, so the answer is the kernel's own and not a version guess.

use std::fmt;
use std::io;
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::os::fd::AsRawFd;

const TCP_ULP: libc::c_int = 31;
const SOL_TLS: libc::c_int = 282;
const TLS_TX: libc::c_int = 1;
const TLS_1_3_VERSION: u16 = 0x0304;
const TLS_CIPHER_AES_GCM_128: u16 = 51;
const TLS_CIPHER_AES_GCM_256: u16 = 52;

/// `struct tls12_crypto_info_aes_gcm_*` of `<linux/tls.h>`: version, cipher, 8 bytes of IV, the key, 4 bytes of salt
/// and 8 bytes of record sequence number.
fn crypto_info(cipher: u16, key: &[u8]) -> Vec<u8> {
    let mut info = Vec::with_capacity(4 + 8 + key.len() + 4 + 8);
    info.extend_from_slice(&TLS_1_3_VERSION.to_ne_bytes());
    info.extend_from_slice(&cipher.to_ne_bytes());
    info.extend_from_slice(&[0; 8]);
    info.extend_from_slice(key);
    info.extend_from_slice(&[0; 4 + 8]);
    info
}

/// Why the kernel cannot send this process's TLS records.
#[derive(Debug)]
pub enum KtlsUnavailable {
    /// The probe could not make a loopback connection.
    Probe(io::Error),
    /// `TCP_ULP` `tls` is not offered: the `tls` module is not loaded (`modprobe tls`; an unprivileged process cannot
    /// load it) or the kernel was built without it.
    UlpMissing(io::Error),
    /// The kernel refused the keys of a cipher.
    Cipher(&'static str, io::Error),
    /// The kernel refused a second key for a socket: it cannot follow a TLS 1.3 `KeyUpdate` (kernel older than 6.5).
    Rekey(&'static str, io::Error),
}

impl fmt::Display for KtlsUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Probe(error) => write!(formatter, "the kernel probe could not run: {error}"),
            Self::UlpMissing(error) => write!(
                formatter,
                "the kernel does not offer the tls upper layer protocol ({error}): load the tls module on the host"
            ),
            Self::Cipher(cipher, error) => write!(formatter, "the kernel refused {cipher} keys: {error}"),
            Self::Rekey(cipher, error) => write!(
                formatter,
                "the kernel refused to replace {cipher} keys on a live socket (TLS 1.3 KeyUpdate needs kernel 6.5): {error}"
            ),
        }
    }
}

impl std::error::Error for KtlsUnavailable {}

fn loopback_pair() -> io::Result<TcpStream> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let _client = TcpStream::connect(listener.local_addr()?)?;
    Ok(listener.accept()?.0)
}

fn set_option(
    socket: &TcpStream,
    level: libc::c_int,
    name: libc::c_int,
    value: &[u8],
) -> io::Result<()> {
    // SAFETY: `value` is a live buffer of the length passed, the descriptor belongs to `socket`.
    let result = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            level,
            name,
            value.as_ptr().cast(),
            value.len() as libc::socklen_t,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn try_cipher(name: &'static str, cipher: u16, key_len: usize) -> Result<(), KtlsUnavailable> {
    let socket = loopback_pair().map_err(KtlsUnavailable::Probe)?;
    set_option(&socket, libc::IPPROTO_TCP, TCP_ULP, b"tls\0")
        .map_err(KtlsUnavailable::UlpMissing)?;
    set_option(
        &socket,
        SOL_TLS,
        TLS_TX,
        &crypto_info(cipher, &vec![1; key_len]),
    )
    .map_err(|error| KtlsUnavailable::Cipher(name, error))?;
    set_option(
        &socket,
        SOL_TLS,
        TLS_TX,
        &crypto_info(cipher, &vec![2; key_len]),
    )
    .map_err(|error| KtlsUnavailable::Rekey(name, error))
}

/// Asks the kernel whether it sends TLS 1.3 records for AES-128-GCM and AES-256-GCM sockets and follows a `KeyUpdate`.
///
/// # Errors
///
/// The first thing the kernel refuses.
pub fn probe() -> Result<(), KtlsUnavailable> {
    try_cipher("AES-128-GCM", TLS_CIPHER_AES_GCM_128, 16)?;
    try_cipher("AES-256-GCM", TLS_CIPHER_AES_GCM_256, 32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_crypto_info_has_the_kernel_struct_size() {
        assert_eq!(crypto_info(TLS_CIPHER_AES_GCM_128, &[0; 16]).len(), 40);
        assert_eq!(crypto_info(TLS_CIPHER_AES_GCM_256, &[0; 32]).len(), 56);
    }

    #[test]
    fn the_probe_names_what_the_kernel_refused() {
        match probe() {
            Ok(()) => {}
            Err(KtlsUnavailable::UlpMissing(_)) => {
                eprintln!("skipped: the tls module is not loaded")
            }
            Err(other) => panic!("unexpected probe failure: {other}"),
        }
    }
}
