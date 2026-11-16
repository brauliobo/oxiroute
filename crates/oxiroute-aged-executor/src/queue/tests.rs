//! The executor's ready queue on a clock the test controls; the order itself is tested with the queue in
//! `oxiroute-aged-order`.

use std::{sync::Arc, time::Duration};

use super::*;

fn tasks_ready(shared: &Arc<Shared>, count: usize) -> Vec<Arc<TaskWaker>> {
    (0..count)
        .map(|id| {
            let task = Arc::new(TaskWaker::new(id, Arc::clone(shared)));
            task.queued.store(true, Ordering::Release);
            shared.push_ready(Arc::clone(&task));
            task
        })
        .collect()
}

#[test]
fn the_newest_ready_task_is_served_until_the_oldest_has_waited_for_the_age() {
    let age = Duration::from_millis(5);
    let shared = Arc::new(Shared::new(Age::Fixed(age), false));
    let before = Instant::now();
    let tasks = tasks_ready(&shared, 3);

    let newest = shared.pop(before + age / 2).unwrap();
    assert_eq!(newest.id, tasks[2].id);
    let oldest = shared.pop(Instant::now() + age).unwrap();
    assert_eq!(oldest.id, tasks[0].id);
    assert_eq!(shared.stats().aged_polls, 1);
}
