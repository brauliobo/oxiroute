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

//! Transport security implemented outside Pingora.
//!
//! A listening endpoint can hand every accepted connection to a [`StreamTlsAcceptor`] instead of the TLS library
//! Pingora was compiled with. The acceptor completes the handshake and returns the established stream.

use crate::offload::OffloadRuntime;
use crate::protocols::{l4::stream::Stream as L4Stream, Stream, UniqueID};
use async_trait::async_trait;
use pingora_error::{ErrorType::InternalError, OrErr, Result};
use std::sync::Arc;

/// Completes the transport security handshake of an accepted connection.
#[async_trait]
pub trait StreamTlsAcceptor: Send + Sync + 'static {
    /// Returns the established stream. The accepted stream keeps the connection admission it carries, so it
    /// must stay inside the returned stream.
    async fn accept(&self, stream: L4Stream) -> Result<Stream>;
}

/// The custom TLS settings of a listening endpoint
pub struct CustomTlsSettings {
    acceptor: Arc<dyn StreamTlsAcceptor>,
    offload_threadpool: Option<(usize, usize)>,
}

impl CustomTlsSettings {
    /// Create settings that run every handshake on the listener's own runtime.
    pub fn new(acceptor: Arc<dyn StreamTlsAcceptor>) -> Self {
        Self {
            acceptor,
            offload_threadpool: None,
        }
    }

    /// Run the handshakes of this endpoint on dedicated single-threaded runtime pools, as
    /// [`TlsSettings::set_offload_threadpool`](crate::listeners::tls::TlsSettings) does for the built-in backend.
    ///
    /// # Panics
    ///
    /// Panics when either `shards` or `threads_per_shard` is zero.
    #[track_caller]
    pub fn set_offload_threadpool(&mut self, shards: usize, threads_per_shard: usize) {
        assert!(shards != 0, "shards must be greater than zero");
        assert!(
            threads_per_shard != 0,
            "threads_per_shard must be greater than zero"
        );
        self.offload_threadpool = Some((shards, threads_per_shard));
    }

    pub(crate) fn build(self) -> CustomTlsAcceptor {
        CustomTlsAcceptor {
            acceptor: self.acceptor,
            offload: self.offload_threadpool.map(|(shards, threads_per_shard)| {
                OffloadRuntime::new("downstream TLS offload", shards, threads_per_shard)
            }),
        }
    }
}

pub(crate) struct CustomTlsAcceptor {
    acceptor: Arc<dyn StreamTlsAcceptor>,
    offload: Option<OffloadRuntime>,
}

impl CustomTlsAcceptor {
    pub(crate) async fn tls_handshake(&self, stream: L4Stream) -> Result<Stream> {
        let Some(offload) = self.offload.as_ref() else {
            return self.acceptor.accept(stream).await;
        };
        let acceptor = self.acceptor.clone();
        offload
            .spawn(
                stream.id() as u64,
                async move { acceptor.accept(stream).await },
            )
            .await
            .or_err(InternalError, "TLS offload runtime failure")?
    }
}
