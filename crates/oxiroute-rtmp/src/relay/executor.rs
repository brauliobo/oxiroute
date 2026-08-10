use std::{
    sync::{
        Arc, Mutex,
        mpsc::{self, SyncSender, TrySendError},
    },
    thread,
};

pub(super) struct BoundedExecutor<T> {
    sender: SyncSender<T>,
}

impl<T: Send + 'static> BoundedExecutor<T> {
    pub(super) fn new(
        queue_capacity: usize,
        worker_count: usize,
        worker_name: &str,
        poisoned_message: &'static str,
        spawn_message: &'static str,
        run: fn(&T),
    ) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<T>(queue_capacity);
        let receiver = Arc::new(Mutex::new(receiver));
        for index in 0..worker_count {
            let receiver = Arc::clone(&receiver);
            thread::Builder::new()
                .name(format!("{worker_name}-{index}"))
                .spawn(move || {
                    loop {
                        let task = receiver.lock().expect(poisoned_message).recv();
                        let Ok(task) = task else {
                            return;
                        };
                        run(&task);
                    }
                })
                .expect(spawn_message);
        }
        Self { sender }
    }

    pub(super) fn admit(&self, task: T) -> Result<(), ()> {
        self.sender.try_send(task).map_err(|error| match error {
            TrySendError::Full(_) | TrySendError::Disconnected(_) => (),
        })
    }
}
