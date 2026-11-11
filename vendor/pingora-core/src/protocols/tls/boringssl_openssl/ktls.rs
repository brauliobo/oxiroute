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

//! Kernel TLS (kTLS) transmit offload for an established OpenSSL TLS 1.3 server connection.
//!
//! OpenSSL installs the write keys in the kernel by itself, but only through a socket BIO, and the accepted stream
//! reads and writes through a BIO that forwards to the transport. A connection is therefore converted on demand: the
//! write BIO becomes a socket BIO, and a TLS 1.3 `KeyUpdate` makes OpenSSL derive the next write key and hand it to
//! the kernel (`TCP_ULP` `tls`, `TLS_TX`). The read direction stays in user space.

pub(super) use imp::*;

#[cfg(feature = "openssl")]
mod imp {
    use std::os::fd::RawFd;
    use std::os::raw::{c_int, c_long, c_void};

    use foreign_types::ForeignTypeRef;

    use crate::tls::ssl::SslRef;
    use crate::tls::ssl_sys::{BIO, SSL};

    unsafe extern "C" {
        fn BIO_new_socket(fd: c_int, close_flag: c_int) -> *mut BIO;
        fn BIO_up_ref(bio: *mut BIO) -> c_int;
        fn BIO_ctrl(bio: *mut BIO, cmd: c_int, larg: c_long, parg: *mut c_void) -> c_long;
        fn SSL_get_rbio(ssl: *const SSL) -> *mut BIO;
        fn SSL_get_wbio(ssl: *const SSL) -> *mut BIO;
        fn SSL_set0_wbio(ssl: *mut SSL, bio: *mut BIO);
        fn SSL_set_options(ssl: *mut SSL, options: u64) -> u64;
        fn SSL_clear_options(ssl: *mut SSL, options: u64) -> u64;
        fn SSL_key_update(ssl: *mut SSL, update_type: c_int) -> c_int;
        fn SSL_want(ssl: *const SSL) -> c_int;
        fn SSL_in_init(ssl: *const SSL) -> c_int;
        fn SSL_get_key_update_type(ssl: *const SSL) -> c_int;
    }

    const BIO_NOCLOSE: c_int = 0;
    const BIO_CTRL_GET_KTLS_SEND: c_int = 73;
    const SSL_OP_ENABLE_KTLS: u64 = 1 << 3;
    const SSL_KEY_UPDATE_NOT_REQUESTED: c_int = 0;
    const SSL_KEY_UPDATE_NONE: c_int = -1;
    const SSL_WRITING: c_int = 2;

    /// Whether the kernel sends this connection's records: `SSL_write` then hands plaintext to the socket.
    pub(crate) fn tx_active(ssl: &SslRef) -> bool {
        // SAFETY: the SSL is alive for the borrow and a socket BIO answers the query without side effects.
        unsafe {
            BIO_ctrl(
                SSL_get_wbio(ssl.as_ptr()),
                BIO_CTRL_GET_KTLS_SEND,
                0,
                std::ptr::null_mut(),
            ) > 0
        }
    }

    /// Whether a `KeyUpdate` waits to be written: one that `begin_tx` scheduled, or the answer to the peer's request,
    /// which OpenSSL otherwise sends only with the next `SSL_write`. The answer is made schedulable here, so that
    /// `SSL_do_handshake` writes it.
    pub(crate) fn key_update_pending(ssl: &SslRef) -> bool {
        // SAFETY: the SSL is alive for the borrow.
        unsafe {
            SSL_in_init(ssl.as_ptr()) != 0
                || (SSL_get_key_update_type(ssl.as_ptr()) != SSL_KEY_UPDATE_NONE
                    && SSL_key_update(ssl.as_ptr(), SSL_KEY_UPDATE_NOT_REQUESTED) == 1)
        }
    }

    /// Whether the last operation on `ssl` stopped because the write BIO could not take more bytes.
    pub(crate) fn wants_write(ssl: &SslRef) -> bool {
        // SAFETY: the SSL is alive for the borrow.
        unsafe { SSL_want(ssl.as_ptr()) == SSL_WRITING }
    }

    /// Starts the conversion of the write direction of `ssl`, whose records go to `fd`: nothing may be buffered in the
    /// stream or the record layer. The `KeyUpdate` is written by the next `SSL_do_handshake`. Returns false when OpenSSL
    /// refused to schedule it (the write BIO is put back).
    pub(crate) fn begin_tx(ssl: &SslRef, fd: RawFd) -> bool {
        // SAFETY: the SSL is alive for the borrow. The socket BIO does not own the descriptor, which outlives the SSL.
        let scheduled = unsafe {
            let socket = BIO_new_socket(fd, BIO_NOCLOSE);
            if socket.is_null() {
                return false;
            }
            SSL_set0_wbio(ssl.as_ptr(), socket);
            SSL_set_options(ssl.as_ptr(), SSL_OP_ENABLE_KTLS);
            SSL_key_update(ssl.as_ptr(), SSL_KEY_UPDATE_NOT_REQUESTED) == 1
        };
        if !scheduled {
            restore_wbio(ssl);
        }
        scheduled
    }

    /// Lets OpenSSL hand the keys of new record layers to the kernel (`SSL_OP_ENABLE_KTLS`) or not. It is on for
    /// everything that writes and off while reading: the read BIO is the forwarding BIO, which answers every control
    /// command it does not know with success, so a read layer that asked it for kernel TLS would be told yes, and
    /// the next peer `KeyUpdate` would leave OpenSSL expecting plaintext from a socket that still sends records.
    pub(crate) fn offer_keys(ssl: &SslRef, offered: bool) {
        // SAFETY: the SSL is alive for the borrow, and the option is only read when a record layer is created.
        unsafe {
            if offered {
                SSL_set_options(ssl.as_ptr(), SSL_OP_ENABLE_KTLS);
            } else {
                SSL_clear_options(ssl.as_ptr(), SSL_OP_ENABLE_KTLS);
            }
        }
    }

    /// Puts the BIO that forwards to the transport back as the write BIO: the connection stays in user space.
    pub(crate) fn restore_wbio(ssl: &SslRef) {
        // SAFETY: the read BIO is the forwarding BIO. It is shared by both directions again, so it gets the second
        // reference that `SSL_set_bio` would have taken; `SSL_set0_wbio` releases the socket BIO.
        unsafe {
            let forwarding = SSL_get_rbio(ssl.as_ptr());
            BIO_up_ref(forwarding);
            SSL_set0_wbio(ssl.as_ptr(), forwarding);
        }
        offer_keys(ssl, false);
    }
}

/// The BoringSSL build has no kernel TLS: connections never convert.
#[cfg(not(feature = "openssl"))]
mod imp {
    use crate::tls::ssl::SslRef;
    use std::os::fd::RawFd;

    pub(crate) fn tx_active(_: &SslRef) -> bool {
        false
    }

    pub(crate) fn wants_write(_: &SslRef) -> bool {
        false
    }

    pub(crate) fn key_update_pending(_: &SslRef) -> bool {
        false
    }

    pub(crate) fn begin_tx(_: &SslRef, _: RawFd) -> bool {
        false
    }

    pub(crate) fn restore_wbio(_: &SslRef) {}

    pub(crate) fn offer_keys(_: &SslRef, _: bool) {}
}
