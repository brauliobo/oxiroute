//! The future that counts a connection on its worker adds a few words to the connection's future, not a copy of it.

use super::*;
use std::mem::size_of_val;

fn guard(pools: &Pools) -> LoadGuard {
    LoadGuard {
        pools: pools.clone(),
        index: AtomicUsize::new(0),
    }
}

fn pools() -> (Pools, tokio::runtime::Runtime) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let pool = Pool {
        handle: runtime.handle().clone(),
        load: Arc::new(AtomicUsize::new(1)),
        executor: None,
    };
    let table = Table {
        pools: vec![pool].into(),
        scaling: None,
    };
    let pools = Arc::new(OnceCell::new());
    assert!(pools.set(table).is_ok());
    (pools, runtime)
}

async fn connection(state: [u8; 1024]) {
    tokio::task::yield_now().await;
    assert_eq!(state.len(), 1024);
}

#[test]
fn counting_a_connection_does_not_copy_its_future() {
    let (pools, _runtime) = pools();
    let future = connection([0; 1024]);
    let alone = size_of_val(&future);
    let counted = Counted::new(guard(&pools), future);
    assert!(
        size_of_val(&counted) <= alone + 64,
        "{} bytes for a future of {alone}",
        size_of_val(&counted)
    );
}

#[test]
fn the_connection_is_counted_until_its_future_is_dropped() {
    let (pools, runtime) = pools();
    let counted = Counted::new(guard(&pools), connection([0; 1024]));
    runtime.block_on(counted);
    assert_eq!(pools.get().unwrap()[0].load.load(Ordering::Relaxed), 0);
}
