//! Addresses of a DNS endpoint that resolves on connect, shared by every request until they expire.

use std::{io, net::SocketAddr, sync::Arc, time::Duration};

use arc_swap::ArcSwapOption;
use tokio::time::Instant;

/// How long one lookup serves every request of the endpoint. A failed connection ends it early.
pub(super) const RESOLUTION_TTL: Duration = Duration::from_secs(1);

#[derive(Debug)]
struct Resolution {
    addresses: Arc<[SocketAddr]>,
    expires: Instant,
}

/// The latest lookup of one endpoint. Requests read it without locking; the first one after it
/// expires looks up again while the others wait for that answer.
#[derive(Debug, Default)]
pub(super) struct ResolvedAddresses {
    current: ArcSwapOption<Resolution>,
    refreshing: tokio::sync::Mutex<()>,
}

impl ResolvedAddresses {
    pub(super) fn fresh(&self) -> Option<Arc<[SocketAddr]>> {
        let current = self.current.load();
        let resolution = current.as_ref()?;
        (Instant::now() < resolution.expires).then(|| Arc::clone(&resolution.addresses))
    }

    pub(super) async fn get_or_resolve<Lookup>(
        &self,
        lookup: impl FnOnce() -> Lookup,
    ) -> io::Result<Arc<[SocketAddr]>>
    where
        Lookup: Future<Output = io::Result<Arc<[SocketAddr]>>>,
    {
        if let Some(addresses) = self.fresh() {
            return Ok(addresses);
        }
        let _refreshing = self.refreshing.lock().await;
        if let Some(addresses) = self.fresh() {
            return Ok(addresses);
        }
        let addresses = lookup().await?;
        self.store(&addresses);
        Ok(addresses)
    }

    pub(super) fn store(&self, addresses: &Arc<[SocketAddr]>) {
        self.current.store(Some(Arc::new(Resolution {
            addresses: Arc::clone(addresses),
            expires: Instant::now() + RESOLUTION_TTL,
        })));
    }

    /// Forgets the lookup, so the next request resolves again.
    pub(super) fn invalidate(&self) {
        self.current.store(None);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn answer() -> Arc<[SocketAddr]> {
        Arc::from([SocketAddr::from(([192, 0, 2, 1], 443))])
    }

    async fn counted(resolved: &ResolvedAddresses, lookups: &AtomicUsize) -> io::Result<()> {
        resolved
            .get_or_resolve(|| async {
                lookups.fetch_add(1, Ordering::SeqCst);
                tokio::task::yield_now().await;
                Ok(answer())
            })
            .await
            .map(drop)
    }

    #[tokio::test]
    async fn requests_share_one_lookup_until_it_expires() {
        let (resolved, lookups) = (ResolvedAddresses::default(), AtomicUsize::new(0));

        for _ in 0..5 {
            counted(&resolved, &lookups).await.expect("lookup");
        }

        assert_eq!(lookups.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_lookup_older_than_the_ttl_is_replaced() {
        let (resolved, lookups) = (ResolvedAddresses::default(), AtomicUsize::new(0));
        counted(&resolved, &lookups).await.expect("first");

        tokio::time::advance(RESOLUTION_TTL.saturating_sub(Duration::from_millis(1))).await;
        counted(&resolved, &lookups).await.expect("still fresh");
        assert_eq!(lookups.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_millis(2)).await;
        counted(&resolved, &lookups).await.expect("expired");

        assert_eq!(lookups.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn concurrent_requests_wait_for_the_one_lookup() {
        let (resolved, lookups) = (
            Arc::new(ResolvedAddresses::default()),
            Arc::new(AtomicUsize::new(0)),
        );

        let requests = (0..16)
            .map(|_| {
                let (resolved, lookups) = (Arc::clone(&resolved), Arc::clone(&lookups));
                tokio::spawn(async move { counted(&resolved, &lookups).await })
            })
            .collect::<Vec<_>>();
        for request in requests {
            request.await.expect("task").expect("lookup");
        }

        assert_eq!(lookups.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn invalidating_makes_the_next_request_look_up_again() {
        let (resolved, lookups) = (ResolvedAddresses::default(), AtomicUsize::new(0));
        counted(&resolved, &lookups).await.expect("first");

        resolved.invalidate();
        counted(&resolved, &lookups).await.expect("second");

        assert_eq!(lookups.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_failed_lookup_is_reported_and_not_kept() {
        let resolved = ResolvedAddresses::default();

        let failure = resolved
            .get_or_resolve(|| async { Err(io::Error::from(io::ErrorKind::NotFound)) })
            .await;
        assert_eq!(
            failure.expect_err("failure").kind(),
            io::ErrorKind::NotFound
        );

        assert!(resolved.fresh().is_none());
    }
}
