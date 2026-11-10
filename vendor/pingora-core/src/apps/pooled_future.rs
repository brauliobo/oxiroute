//! A boxed future whose heap block is kept by the thread for the next one.
//!
//! The request future of a proxy is 11 KiB. Allocating and freeing a block of that size for every request makes the
//! allocator walk its slabs (a measured 5% of the cache misses per request at 2,048 connections), whereas a block
//! the thread just gave back is still in its cache.

use std::alloc::{alloc, dealloc, handle_alloc_error, Layout};
use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::ptr::NonNull;
use std::task::{Context, Poll};

/// Blocks kept per thread: as many requests as were in flight at the same time and then some.
const POOLED_BLOCKS: usize = 64;

thread_local! {
    static BLOCKS: RefCell<Vec<(Layout, NonNull<u8>)>> = const { RefCell::new(Vec::new()) };
}

pub(super) struct PooledFuture<F> {
    future: NonNull<F>,
}

// SAFETY: it owns its future like a `Box<F>` does.
unsafe impl<F: Send> Send for PooledFuture<F> {}
unsafe impl<F: Sync> Sync for PooledFuture<F> {}

impl<F: Future> PooledFuture<F> {
    /// The future is written where it will live: the block is taken before `make` runs.
    #[inline(always)]
    pub(super) fn new(make: impl FnOnce() -> F) -> Self {
        let layout = Layout::new::<F>();
        let block = BLOCKS
            .with_borrow_mut(|blocks| {
                let at = blocks.iter().rposition(|(held, _)| *held == layout)?;
                Some(blocks.swap_remove(at).1)
            })
            .unwrap_or_else(|| {
                // SAFETY: a future is never zero-sized here (it holds its arguments).
                NonNull::new(unsafe { alloc(layout) }).unwrap_or_else(|| handle_alloc_error(layout))
            });
        let future = block.cast::<F>();
        // SAFETY: the block is valid for `layout` and owned by this value.
        unsafe { future.write(make()) };
        Self { future }
    }
}

impl<F> Drop for PooledFuture<F> {
    fn drop(&mut self) {
        let layout = Layout::new::<F>();
        // SAFETY: the future was written in `new` and is dropped once; its block has `layout`.
        unsafe { self.future.drop_in_place() };
        let block = self.future.cast::<u8>();
        let kept = BLOCKS.try_with(|blocks| {
            let mut blocks = blocks.borrow_mut();
            let room = blocks.len() < POOLED_BLOCKS;
            if room {
                blocks.push((layout, block));
            }
            room
        });
        if !kept.unwrap_or(false) {
            // SAFETY: allocated in `new` with this layout and not kept by the pool.
            unsafe { dealloc(block.as_ptr(), layout) };
        }
    }
}

impl<F: Future> Future for PooledFuture<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: the future lives in its own heap block until this value is dropped and never moves.
        unsafe { Pin::new_unchecked(&mut *self.get_unchecked_mut().future.as_ptr()) }.poll(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn big() -> impl Future<Output = usize> {
        async {
            let state = [7_u8; 4096];
            tokio::task::yield_now().await;
            state.iter().map(|byte| usize::from(*byte)).sum()
        }
    }

    #[tokio::test]
    async fn the_future_runs_to_completion_in_its_block() {
        assert_eq!(PooledFuture::new(big).await, 4096 * 7);
    }

    #[tokio::test]
    async fn the_block_of_a_finished_future_serves_the_next_one() {
        let first = PooledFuture::new(big);
        let address = first.future.as_ptr() as usize;
        first.await;
        let second = PooledFuture::new(big);
        assert_eq!(second.future.as_ptr() as usize, address);
    }

    #[tokio::test]
    async fn a_future_dropped_before_it_finished_drops_what_it_holds() {
        struct Counted(Arc<AtomicUsize>);
        impl Drop for Counted {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let drops = Arc::new(AtomicUsize::new(0));
        let held = Counted(drops.clone());
        let mut future = Box::pin(PooledFuture::new(move || async move {
            let _held = held;
            std::future::pending::<()>().await;
        }));
        assert!(futures::poll!(future.as_mut()).is_pending());
        drop(future);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn the_pool_keeps_a_bounded_number_of_blocks() {
        let futures: Vec<_> = (0..POOLED_BLOCKS * 2)
            .map(|_| PooledFuture::new(|| async { 1_usize }))
            .collect();
        drop(futures);
        assert_eq!(BLOCKS.with_borrow(Vec::len), POOLED_BLOCKS);
    }
}
