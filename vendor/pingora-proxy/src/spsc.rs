use futures::task::AtomicWaker;
use std::{
    array,
    error::Error,
    fmt,
    future::Future,
    mem,
    pin::Pin,
    sync::Mutex,
    task::{Context, Poll},
};

/// A bounded, stack-owned single-producer/single-consumer channel.
///
/// The borrowed endpoints are not cloneable. `Sender` operations take shared references so a
/// permit can coexist with the select loop that owns the sender, but only one reservation future
/// may be pending. A second reservation completes with `ReserveError::Concurrent` without
/// replacing the first waiter's waker.
pub(crate) struct Channel<T, const CAPACITY: usize> {
    state: Mutex<State<T, CAPACITY>>,
    sender_waker: AtomicWaker,
    receiver_waker: AtomicWaker,
}

struct State<T, const CAPACITY: usize> {
    queue: [Option<T>; CAPACITY],
    head: usize,
    len: usize,
    reserved: bool,
    reserve_waiting: bool,
    sender_open: bool,
    receiver_alive: bool,
    receiver_accepting: bool,
}

pub(crate) struct Sender<'a, T, const CAPACITY: usize> {
    channel: &'a Channel<T, CAPACITY>,
}

pub(crate) struct Receiver<'a, T, const CAPACITY: usize> {
    channel: &'a Channel<T, CAPACITY>,
}

pub(crate) struct Permit<'a, T, const CAPACITY: usize> {
    channel: &'a Channel<T, CAPACITY>,
    used: bool,
}

impl<T, const CAPACITY: usize> fmt::Debug for Permit<'_, T, CAPACITY> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Permit(..)")
    }
}

pub(crate) struct Reserve<'a, T, const CAPACITY: usize> {
    channel: &'a Channel<T, CAPACITY>,
    waiting: bool,
}

pub(crate) struct Recv<'a, T, const CAPACITY: usize> {
    channel: &'a Channel<T, CAPACITY>,
}

pub(crate) struct SendError<T>(pub(crate) T, pub(crate) ReserveError);

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ReserveError {
    Closed,
    Concurrent,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum TryReserveError {
    Full,
    Closed,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum TryRecvError {
    Empty,
    Closed,
}

impl<T, const CAPACITY: usize> Channel<T, CAPACITY> {
    pub(crate) fn new() -> Self {
        assert!(CAPACITY > 0, "SPSC channel capacity must be nonzero");
        Self {
            state: Mutex::new(State {
                queue: array::from_fn(|_| None),
                head: 0,
                len: 0,
                reserved: false,
                reserve_waiting: false,
                sender_open: true,
                receiver_alive: true,
                receiver_accepting: true,
            }),
            sender_waker: AtomicWaker::new(),
            receiver_waker: AtomicWaker::new(),
        }
    }

    pub(crate) fn split(&mut self) -> (Sender<'_, T, CAPACITY>, Receiver<'_, T, CAPACITY>) {
        let channel = &*self;
        (Sender { channel }, Receiver { channel })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State<T, CAPACITY>> {
        self.state.lock().expect("SPSC channel mutex poisoned")
    }
}

impl<T, const CAPACITY: usize> Sender<'_, T, CAPACITY> {
    pub(crate) async fn send(&self, value: T) -> Result<(), SendError<T>> {
        let permit = match self.reserve().await {
            Ok(permit) => permit,
            Err(error) => return Err(SendError(value, error)),
        };
        permit.send(value);
        Ok(())
    }

    pub(crate) fn try_reserve(&self) -> Result<Permit<'_, T, CAPACITY>, TryReserveError> {
        let mut state = self.channel.state();
        if !state.sender_open || !state.receiver_alive || !state.receiver_accepting {
            return Err(TryReserveError::Closed);
        }
        if state.reserve_waiting || state.reserved || state.len == CAPACITY {
            return Err(TryReserveError::Full);
        }
        state.reserved = true;
        Ok(Permit {
            channel: self.channel,
            used: false,
        })
    }

    pub(crate) fn reserve(&self) -> Reserve<'_, T, CAPACITY> {
        Reserve {
            channel: self.channel,
            waiting: false,
        }
    }

    pub(crate) fn is_closed(&self) -> bool {
        let state = self.channel.state();
        !state.sender_open || !state.receiver_alive || !state.receiver_accepting
    }

    pub(crate) fn close(&self) {
        let wake = {
            let mut state = self.channel.state();
            if !state.sender_open {
                false
            } else {
                state.sender_open = false;
                true
            }
        };
        if wake {
            self.channel.sender_waker.wake();
            self.channel.receiver_waker.wake();
        }
    }
}

impl<T, const CAPACITY: usize> Drop for Sender<'_, T, CAPACITY> {
    fn drop(&mut self) {
        self.close();
    }
}

impl<T, const CAPACITY: usize> Receiver<'_, T, CAPACITY> {
    pub(crate) fn recv(&mut self) -> Recv<'_, T, CAPACITY> {
        Recv {
            channel: self.channel,
        }
    }

    pub(crate) fn try_recv(&mut self) -> Result<T, TryRecvError> {
        let result = {
            let mut state = self.channel.state();
            if state.len > 0 {
                Ok(state.pop())
            } else if state.recv_closed() {
                Err(TryRecvError::Closed)
            } else {
                Err(TryRecvError::Empty)
            }
        };
        if result.is_ok() {
            self.channel.sender_waker.wake();
        }
        result
    }

    #[allow(dead_code, reason = "explicit close is part of the channel contract")]
    pub(crate) fn close(&mut self) {
        let wake = {
            let mut state = self.channel.state();
            if !state.receiver_accepting {
                false
            } else {
                state.receiver_accepting = false;
                true
            }
        };
        if wake {
            self.channel.sender_waker.wake();
            self.channel.receiver_waker.wake();
        }
    }
}

impl<T, const CAPACITY: usize> Drop for Receiver<'_, T, CAPACITY> {
    fn drop(&mut self) {
        let queued = {
            let mut state = self.channel.state();
            if !state.receiver_alive {
                None
            } else {
                state.receiver_alive = false;
                state.receiver_accepting = false;
                state.head = 0;
                state.len = 0;
                Some(mem::replace(&mut state.queue, array::from_fn(|_| None)))
            }
        };
        if let Some(queued) = queued {
            self.channel.sender_waker.wake();
            drop(queued);
        }
    }
}

impl<T, const CAPACITY: usize> Permit<'_, T, CAPACITY> {
    pub(crate) fn send(mut self, value: T) {
        let (wake_sender, wake_receiver) = {
            let mut state = self.channel.state();
            debug_assert!(state.reserved);
            state.reserved = false;
            self.used = true;
            if state.receiver_alive {
                state.push(value);
                (state.reserve_waiting && state.len < CAPACITY, true)
            } else {
                (state.reserve_waiting, false)
            }
        };
        if wake_sender {
            self.channel.sender_waker.wake();
        }
        if wake_receiver {
            self.channel.receiver_waker.wake();
        }
    }
}

impl<T, const CAPACITY: usize> Drop for Permit<'_, T, CAPACITY> {
    fn drop(&mut self) {
        if self.used {
            return;
        }
        {
            let mut state = self.channel.state();
            debug_assert!(state.reserved);
            state.reserved = false;
        }
        self.channel.sender_waker.wake();
        self.channel.receiver_waker.wake();
    }
}

impl<'a, T, const CAPACITY: usize> Future for Reserve<'a, T, CAPACITY> {
    type Output = Result<Permit<'a, T, CAPACITY>, ReserveError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.channel.state();
        if !state.sender_open || !state.receiver_alive || !state.receiver_accepting {
            if self.waiting {
                state.reserve_waiting = false;
                self.waiting = false;
            }
            return Poll::Ready(Err(ReserveError::Closed));
        }

        if !self.waiting {
            if state.reserve_waiting {
                return Poll::Ready(Err(ReserveError::Concurrent));
            }
            state.reserve_waiting = true;
            self.waiting = true;
        }

        if !state.reserved && state.len < CAPACITY {
            state.reserve_waiting = false;
            self.waiting = false;
            state.reserved = true;
            return Poll::Ready(Ok(Permit {
                channel: self.channel,
                used: false,
            }));
        }

        self.channel.sender_waker.register(cx.waker());
        Poll::Pending
    }
}

impl<T, const CAPACITY: usize> Drop for Reserve<'_, T, CAPACITY> {
    fn drop(&mut self) {
        if !self.waiting {
            return;
        }
        {
            let mut state = self.channel.state();
            debug_assert!(state.reserve_waiting);
            state.reserve_waiting = false;
        }
        self.channel.sender_waker.wake();
    }
}

impl<T, const CAPACITY: usize> Future for Recv<'_, T, CAPACITY> {
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let result = {
            let mut state = self.channel.state();
            if state.len > 0 {
                Poll::Ready(Some(state.pop()))
            } else if state.recv_closed() {
                Poll::Ready(None)
            } else {
                self.channel.receiver_waker.register(cx.waker());
                Poll::Pending
            }
        };
        if matches!(result, Poll::Ready(Some(_))) {
            self.channel.sender_waker.wake();
        }
        result
    }
}

impl<T, const CAPACITY: usize> State<T, CAPACITY> {
    fn push(&mut self, value: T) {
        debug_assert!(self.len < CAPACITY);
        let tail = (self.head + self.len) % CAPACITY;
        debug_assert!(self.queue[tail].is_none());
        self.queue[tail] = Some(value);
        self.len += 1;
    }

    fn pop(&mut self) -> T {
        debug_assert!(self.len > 0);
        let value = self.queue[self.head]
            .take()
            .expect("occupied SPSC queue slot");
        self.head = (self.head + 1) % CAPACITY;
        self.len -= 1;
        value
    }

    fn recv_closed(&self) -> bool {
        (!self.sender_open || !self.receiver_accepting) && !self.reserved
    }
}

impl<T> fmt::Debug for SendError<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SendError(..)")
    }
}

impl<T> fmt::Display for SendError<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.1.fmt(formatter)
    }
}

impl<T> Error for SendError<T> {}

impl fmt::Display for ReserveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => formatter.write_str("SPSC channel closed"),
            Self::Concurrent => formatter.write_str("concurrent SPSC reservation"),
        }
    }
}

impl Error for ReserveError {}

impl fmt::Display for TryReserveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full => formatter.write_str("SPSC channel full"),
            Self::Closed => formatter.write_str("SPSC channel closed"),
        }
    }
}

impl Error for TryReserveError {}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{
        poll,
        task::{noop_waker_ref, waker, ArcWake},
    };
    use std::{
        panic::{catch_unwind, AssertUnwindSafe},
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        task::Context,
        time::Duration,
    };
    use tokio::time::timeout;

    #[derive(Default)]
    struct WakeCounter(AtomicUsize);

    impl ArcWake for WakeCounter {
        fn wake_by_ref(counter: &Arc<Self>) {
            counter.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl WakeCounter {
        fn count(&self) -> usize {
            self.0.load(Ordering::SeqCst)
        }
    }

    struct PanicOnDrop {
        drops: Arc<AtomicUsize>,
        sender_wakes: Arc<WakeCounter>,
    }

    impl Drop for PanicOnDrop {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                self.sender_wakes.count(),
                1,
                "sender must wake before T drops"
            );
            panic!("adversarial queued value drop");
        }
    }

    #[tokio::test]
    async fn capacity_and_fifo_are_exact() {
        let mut channel = Channel::<usize, 4>::new();
        let (sender, mut receiver) = channel.split();
        for value in 0..4 {
            sender.try_reserve().unwrap().send(value);
        }
        assert_eq!(sender.try_reserve().unwrap_err(), TryReserveError::Full);
        for value in 0..4 {
            assert_eq!(receiver.recv().await, Some(value));
        }
        assert_eq!(receiver.try_recv(), Err(TryRecvError::Empty));
    }

    #[tokio::test]
    async fn pending_reserve_has_priority_over_try_reserve() {
        let mut channel = Channel::<usize, 1>::new();
        let (sender, mut receiver) = channel.split();
        sender.send(1).await.unwrap();

        let mut reserve = Box::pin(sender.reserve());
        assert!(poll!(&mut reserve).is_pending());
        assert_eq!(receiver.recv().await, Some(1));
        assert_eq!(sender.try_reserve().unwrap_err(), TryReserveError::Full);
        reserve.await.unwrap().send(2);
        assert_eq!(receiver.recv().await, Some(2));
    }

    #[tokio::test]
    async fn cancelled_reserve_and_permit_release_capacity() {
        let mut channel = Channel::<usize, 1>::new();
        let (sender, mut receiver) = channel.split();
        sender.send(1).await.unwrap();

        let mut reserve = Box::pin(sender.reserve());
        assert!(poll!(&mut reserve).is_pending());
        drop(reserve);
        assert_eq!(receiver.recv().await, Some(1));

        let permit = sender.try_reserve().unwrap();
        drop(permit);
        sender.try_reserve().unwrap().send(2);
        assert_eq!(receiver.recv().await, Some(2));
    }

    #[test]
    fn sending_reserved_value_wakes_first_waiter_when_capacity_remains() {
        let mut channel = Channel::<usize, 2>::new();
        let (sender, mut receiver) = channel.split();
        let active_permit = sender.try_reserve().unwrap();
        let mut waiting = Box::pin(sender.reserve());
        let wakes = Arc::new(WakeCounter::default());
        let waiter_waker = waker(Arc::clone(&wakes));
        let mut context = Context::from_waker(&waiter_waker);

        assert!(waiting.as_mut().poll(&mut context).is_pending());
        active_permit.send(1);
        assert_eq!(wakes.count(), 1);

        let Poll::Ready(Ok(permit)) = waiting.as_mut().poll(&mut context) else {
            panic!("released reservation did not wake the first waiter");
        };
        permit.send(2);
        assert_eq!(receiver.try_recv(), Ok(1));
        assert_eq!(receiver.try_recv(), Ok(2));
    }

    #[test]
    fn cancelling_permit_wakes_pending_reservation() {
        let mut channel = Channel::<usize, 1>::new();
        let (sender, _receiver) = channel.split();
        let active_permit = sender.try_reserve().unwrap();
        let mut waiting = Box::pin(sender.reserve());
        let wakes = Arc::new(WakeCounter::default());
        let waiter_waker = waker(Arc::clone(&wakes));
        let mut context = Context::from_waker(&waiter_waker);

        assert!(waiting.as_mut().poll(&mut context).is_pending());
        drop(active_permit);
        assert_eq!(wakes.count(), 1);
        assert!(matches!(
            waiting.as_mut().poll(&mut context),
            Poll::Ready(Ok(_))
        ));
    }

    #[test]
    fn concurrent_reservation_preserves_first_waiter_and_waker() {
        let mut channel = Channel::<usize, 1>::new();
        let (sender, mut receiver) = channel.split();
        sender.try_reserve().unwrap().send(1);
        let first_wakes = Arc::new(WakeCounter::default());
        let second_wakes = Arc::new(WakeCounter::default());
        let first_waker = waker(Arc::clone(&first_wakes));
        let second_waker = waker(Arc::clone(&second_wakes));
        let mut first_context = Context::from_waker(&first_waker);
        let mut second_context = Context::from_waker(&second_waker);
        let mut first = Box::pin(sender.reserve());
        let mut second = Box::pin(sender.reserve());

        assert!(first.as_mut().poll(&mut first_context).is_pending());
        assert!(matches!(
            second.as_mut().poll(&mut second_context),
            Poll::Ready(Err(ReserveError::Concurrent))
        ));
        assert_eq!(first_wakes.count(), 0);
        assert_eq!(second_wakes.count(), 0);

        assert_eq!(receiver.try_recv(), Ok(1));
        assert_eq!(first_wakes.count(), 1);
        assert_eq!(second_wakes.count(), 0);
        let Poll::Ready(Ok(permit)) = first.as_mut().poll(&mut first_context) else {
            panic!("first reservation lost ownership of the waiter slot");
        };
        permit.send(2);
        assert_eq!(receiver.try_recv(), Ok(2));
    }

    #[tokio::test]
    async fn close_and_drop_wake_the_peer() {
        let mut first = Channel::<usize, 1>::new();
        let (sender, mut receiver) = first.split();
        let waiting_receiver = receiver.recv();
        sender.close();
        assert_eq!(
            timeout(Duration::from_millis(100), waiting_receiver).await,
            Ok(None)
        );

        let mut second = Channel::<usize, 1>::new();
        let (sender, mut receiver) = second.split();
        sender.send(1).await.unwrap();
        let mut reserve = Box::pin(sender.reserve());
        assert!(poll!(&mut reserve).is_pending());
        receiver.close();
        assert_eq!(reserve.await.unwrap_err(), ReserveError::Closed);
        assert_eq!(receiver.recv().await, Some(1));
        assert_eq!(receiver.recv().await, None);

        let mut third = Channel::<usize, 1>::new();
        let (sender, receiver) = third.split();
        drop(receiver);
        assert_eq!(sender.reserve().await.unwrap_err(), ReserveError::Closed);
    }

    #[test]
    fn sender_close_wakes_reservation_and_receiver_with_outstanding_permit() {
        let mut channel = Channel::<usize, 1>::new();
        let (sender, mut receiver) = channel.split();
        let permit = sender.try_reserve().unwrap();
        let sender_wakes = Arc::new(WakeCounter::default());
        let receiver_wakes = Arc::new(WakeCounter::default());
        let sender_waker = waker(Arc::clone(&sender_wakes));
        let receiver_waker = waker(Arc::clone(&receiver_wakes));
        let mut sender_context = Context::from_waker(&sender_waker);
        let mut receiver_context = Context::from_waker(&receiver_waker);
        let mut reservation = Box::pin(sender.reserve());
        let mut receive = Box::pin(receiver.recv());

        assert!(reservation.as_mut().poll(&mut sender_context).is_pending());
        assert!(receive.as_mut().poll(&mut receiver_context).is_pending());
        sender.close();
        assert_eq!(sender_wakes.count(), 1);
        assert_eq!(receiver_wakes.count(), 1);
        assert!(matches!(
            reservation.as_mut().poll(&mut sender_context),
            Poll::Ready(Err(ReserveError::Closed))
        ));
        assert!(receive.as_mut().poll(&mut receiver_context).is_pending());

        permit.send(7);
        assert_eq!(receiver_wakes.count(), 2);
        assert_eq!(
            receive.as_mut().poll(&mut receiver_context),
            Poll::Ready(Some(7))
        );
        drop(receive);
        assert_eq!(receiver.try_recv(), Err(TryRecvError::Closed));
    }

    #[test]
    fn receiver_drop_wakes_before_panicking_value_destructor_and_does_not_poison() {
        let mut channel = Channel::<PanicOnDrop, 1>::new();
        let (sender, receiver) = channel.split();
        let sender_wakes = Arc::new(WakeCounter::default());
        let drops = Arc::new(AtomicUsize::new(0));
        sender.try_reserve().unwrap().send(PanicOnDrop {
            drops: Arc::clone(&drops),
            sender_wakes: Arc::clone(&sender_wakes),
        });
        let waiter_waker = waker(Arc::clone(&sender_wakes));
        let mut context = Context::from_waker(&waiter_waker);
        let mut waiting = Box::pin(sender.reserve());
        assert!(waiting.as_mut().poll(&mut context).is_pending());

        let panic = catch_unwind(AssertUnwindSafe(|| drop(receiver)));
        assert!(panic.is_err());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(sender_wakes.count(), 1);
        assert!(sender.is_closed(), "receiver drop poisoned channel state");
        assert!(matches!(
            waiting.as_mut().poll(&mut context),
            Poll::Ready(Err(ReserveError::Closed))
        ));
    }

    #[tokio::test]
    async fn closed_channel_completes_without_repoll_spin() {
        let mut channel = Channel::<usize, 1>::new();
        let (sender, receiver) = channel.split();
        drop(receiver);

        let polls = Arc::new(AtomicUsize::new(0));
        let mut reserve = Box::pin(sender.reserve());
        let counted = {
            let polls = Arc::clone(&polls);
            futures::future::poll_fn(move |cx| {
                polls.fetch_add(1, Ordering::Relaxed);
                match reserve.as_mut().poll(cx) {
                    Poll::Ready(result) => Poll::Ready(result.map(drop)),
                    Poll::Pending => Poll::Pending,
                }
            })
        };
        assert!(timeout(Duration::from_millis(100), counted)
            .await
            .unwrap()
            .is_err());
        assert_eq!(polls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn try_join_cancellation_drops_sender_and_wakes_receiver() {
        let mut channel = Channel::<usize, 1>::new();
        let (sender, mut receiver) = channel.split();
        let joined = async move {
            let holds_sender = async move {
                let _sender = sender;
                futures::future::pending::<Result<(), ()>>().await
            };
            let fails = async { Err::<(), _>(()) };
            tokio::try_join!(holds_sender, fails)
        };
        assert!(joined.await.is_err());
        assert_eq!(
            timeout(Duration::from_millis(100), receiver.recv()).await,
            Ok(None)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn randomized_yields_do_not_lose_wakeups_or_order() {
        timeout(Duration::from_secs(10), async {
            for seed in 1..=128_u64 {
                tokio::spawn(async move {
                    let mut channel = Channel::<usize, 4>::new();
                    let (sender, mut receiver) = channel.split();
                    let producer = async {
                        let mut random = seed;
                        for value in 0..256 {
                            random ^= random << 13;
                            random ^= random >> 7;
                            random ^= random << 17;
                            if random & 3 == 0 {
                                tokio::task::yield_now().await;
                            }
                            if random & 15 == 0 {
                                drop(sender.reserve().await.unwrap());
                            }
                            match sender.try_reserve() {
                                Ok(permit) => permit.send(value),
                                Err(TryReserveError::Full) => {
                                    sender.send(value).await.unwrap();
                                }
                                Err(TryReserveError::Closed) => panic!("receiver closed early"),
                            }
                        }
                    };
                    let consumer = async {
                        let mut random = !seed;
                        for expected in 0..256 {
                            random ^= random << 13;
                            random ^= random >> 7;
                            random ^= random << 17;
                            if random & 3 == 0 {
                                tokio::task::yield_now().await;
                            }
                            assert_eq!(receiver.recv().await, Some(expected));
                        }
                    };
                    tokio::join!(producer, consumer);
                })
                .await
                .unwrap();
            }
        })
        .await
        .expect("SPSC stress test timed out");
    }

    #[test]
    fn endpoint_futures_are_send() {
        fn assert_send<T: Send>(_: T) {}

        let future = async {
            let mut channel = Channel::<usize, 4>::new();
            let (sender, mut receiver) = channel.split();
            tokio::join!(sender.send(1), receiver.recv())
        };
        assert_send(future);

        let mut channel = Channel::<usize, 1>::new();
        let (sender, _receiver) = channel.split();
        let mut reserve = Box::pin(sender.reserve());
        let mut context = Context::from_waker(noop_waker_ref());
        assert!(reserve.as_mut().poll(&mut context).is_ready());
    }
}
