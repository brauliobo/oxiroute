use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use oxiroute_epoll_h1::{Age, Config, Policy, Scale, Scheduling};

use crate::support::{Fixture, Knobs, Wire, eventually, test_config};

pub const HOUR: Duration = Duration::from_secs(3600);

static NAMES: AtomicUsize = AtomicUsize::new(0);

pub fn plain(threads: usize) -> Config {
    Config {
        threads,
        scheduling: None,
        ..test_config()
    }
}

pub fn aged(threads: usize, age: Age) -> Config {
    Config {
        scheduling: Some(Scheduling { age, scale: None }),
        ..plain(threads)
    }
}

pub fn scaled(threads: usize, min_active: usize, release_delay: Duration) -> Config {
    let name = format!("engine-schedule-{}", NAMES.fetch_add(1, Ordering::Relaxed));
    let policy = Policy {
        min_active,
        target_busy_permille: 200,
        release_delay,
    };
    Config {
        scheduling: Some(Scheduling {
            age: Age::adaptive(),
            scale: Some(Scale { name, policy }),
        }),
        ..plain(threads)
    }
}

pub fn recorded() -> Knobs {
    Knobs {
        record: true,
        ..Knobs::default()
    }
}

pub fn body_for(id: usize, len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| b'a' + ((index * 7 + id * 13) % 26) as u8)
        .collect()
}

pub fn echo(wire: &mut Wire, id: usize, len: usize) {
    let body = body_for(id, len);
    let mut request =
        format!("POST /echo HTTP/1.1\r\nHost: test.example\r\nContent-Length: {len}\r\n\r\n")
            .into_bytes();
    request.extend_from_slice(&body);
    wire.send(&request);
    let reply = wire.reply();
    assert_eq!(reply.status, 200);
    assert!(reply.body == body, "the echo of {id} came back changed");
}

pub fn connections(fixture: &Fixture) -> Vec<usize> {
    fixture.engine().connections()
}

pub fn active(fixture: &Fixture) -> usize {
    fixture
        .engine()
        .scaling_stats()
        .expect("a scaled engine")
        .active
}

pub fn relocations(fixture: &Fixture) -> u64 {
    fixture
        .engine()
        .scaling_stats()
        .expect("a scaled engine")
        .relocations
}

pub fn open(fixture: &Fixture, count: usize) -> Vec<Wire> {
    (0..count)
        .map(|id| {
            let mut wire = fixture.wire();
            echo(&mut wire, id, 16);
            wire
        })
        .collect()
}

pub fn burst(fixture: &Fixture, count: usize) -> Vec<Wire> {
    const CLIENTS: usize = 4;
    let addr = fixture.addr;
    let clients = (0..CLIENTS)
        .map(|_| {
            std::thread::spawn(move || {
                (0..count / CLIENTS)
                    .map(|_| Wire::connect(addr))
                    .collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    let mut wires = clients
        .into_iter()
        .flat_map(|client| client.join().expect("client thread"))
        .collect::<Vec<_>>();
    wires
        .iter_mut()
        .enumerate()
        .for_each(|(id, wire)| echo(wire, id, 16));
    wires
}

pub fn pack(fixture: &Fixture, wires: &mut [Wire], threads: usize) {
    let total = wires.len();
    eventually("the connections to be packed on the first thread", || {
        wires
            .iter_mut()
            .enumerate()
            .for_each(|(id, wire)| echo(wire, id, 64));
        std::thread::sleep(Duration::from_millis(150));
        let held = connections(fixture);
        held.len() == threads && held[0] == total && active(fixture) == 1
    });
}
