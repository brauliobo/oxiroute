use std::{
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::{Arc, atomic::Ordering},
    task::{Context, Poll, Waker},
    time::Instant,
};

use slab::Slab;
use tokio::task::coop::has_budget_remaining;

use crate::queue::{Shared, Task, TaskWaker};

/// Tasks polled before the runner yields to the Tokio scheduler, which then polls the I/O driver. Serving many more
/// ready tasks than that between two polls of the driver turns the queue into first-in first-out.
const BATCH: usize = 32;

struct Slot {
    future: Task,
    task: Arc<TaskWaker>,
    waker: Waker,
}

/// The future that polls the tasks of one executor. It never completes; dropping it drops the tasks.
pub struct Runner {
    shared: Arc<Shared>,
    slots: Slab<Slot>,
}

impl Runner {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            slots: Slab::new(),
        }
    }

    fn admit(&mut self, tasks: Vec<Task>) {
        let spawned = tasks.len();
        for future in tasks {
            let entry = self.slots.vacant_entry();
            let task = Arc::new(TaskWaker::new(entry.key(), Arc::clone(&self.shared)));
            entry.insert(Slot {
                future,
                waker: Waker::from(Arc::clone(&task)),
                task: Arc::clone(&task),
            });
            task.queued.store(true, Ordering::Release);
            self.shared.push_ready(task);
        }
        self.shared.count_tasks(spawned, 0);
    }

    fn run(&mut self, task: &Arc<TaskWaker>) {
        let Some(slot) = self
            .slots
            .get_mut(task.id)
            .filter(|slot| Arc::ptr_eq(&slot.task, task))
        else {
            return;
        };
        task.queued.store(false, Ordering::Release);
        let mut cx = Context::from_waker(&slot.waker);
        let polled = catch_unwind(AssertUnwindSafe(|| slot.future.as_mut().poll(&mut cx)));
        if !matches!(polled, Ok(Poll::Pending)) {
            task.queued.store(true, Ordering::Release);
            let finished = self.slots.remove(task.id);
            self.shared.count_tasks(0, 1);
            drop(finished);
        }
    }
}

impl Future for Runner {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        this.shared.register_runner(cx.waker());
        let now = Instant::now();
        let submitted = this.shared.begin_batch(now);
        this.admit(submitted);
        for _ in 0..BATCH {
            let Some(task) = this.shared.pop(now) else {
                return Poll::Pending;
            };
            this.run(&task);
            if !has_budget_remaining() {
                break;
            }
        }
        if this.shared.has_ready() {
            cx.waker().wake_by_ref();
        }
        Poll::Pending
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        let abandoned = self.shared.close();
        drop(abandoned);
        drop(std::mem::take(&mut self.slots));
    }
}
