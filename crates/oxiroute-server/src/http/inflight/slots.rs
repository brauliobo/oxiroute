use super::*;

fn arrival(request_id: &str) -> InflightArrival<'_> {
    InflightArrival {
        request_id,
        listener: "edge".into(),
        method: Method::POST,
        host: Some("app.test"),
        path: "/v1/render",
        client_ip: None,
        started: Instant::now(),
        deadline: Duration::from_secs(30),
        access_log: None,
    }
}

fn registry_for_test() -> &'static InflightRegistry {
    Box::leak(Box::new(InflightRegistry::new()))
}

fn listed(registry: &InflightRegistry) -> Vec<String> {
    let mut ids = registry
        .list(&InflightFilter::default(), usize::MAX)
        .requests
        .into_iter()
        .map(|view| view.request_id)
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

#[test]
fn the_slot_of_a_finished_request_is_the_next_arrivals_while_the_others_stay_listed() {
    let registry = registry_for_test();
    let mut guards = (0..4)
        .map(|index| Some(registry.register(arrival(&format!("s{index}")))))
        .collect::<Vec<_>>();
    drop(guards[1].take());
    drop(guards[2].take());
    let later = registry.register(arrival("s4"));
    assert_eq!(listed(registry), ["s0", "s3", "s4"]);
    assert!(later.entry.slot() < 4, "the freed slots are filled first");
}

#[test]
fn requests_that_come_and_go_on_many_threads_are_listed_whole_and_leave_nothing_behind() {
    let registry = registry_for_test();
    let scanning = AtomicBool::new(true);
    thread::scope(|scope| {
        let scanner = scope.spawn(|| {
            let mut scans = 0_u32;
            while scanning.load(Ordering::Relaxed) {
                let ids = listed(registry);
                assert!(ids.iter().all(|id| id.starts_with('t')));
                scans += 1;
            }
            scans
        });
        let workers = (0..8)
            .map(|worker| {
                scope.spawn(move || {
                    let mut held = Vec::new();
                    for index in 0..2_000 {
                        let guard = registry.register(arrival(&format!("t{worker}-{index}")));
                        if index % 5 == 0 {
                            held.push(guard);
                        }
                    }
                    let ids = listed(registry);
                    assert!(held.len() == 400 && ids.len() >= 400);
                    held.len()
                })
            })
            .collect::<Vec<_>>();
        let kept = workers
            .into_iter()
            .map(|worker| worker.join().expect("worker"))
            .sum::<usize>();
        scanning.store(false, Ordering::Relaxed);
        assert!(scanner.join().expect("scanner") > 0);
        assert_eq!(kept, 8 * 400);
    });
    assert!(listed(registry).is_empty());
}
