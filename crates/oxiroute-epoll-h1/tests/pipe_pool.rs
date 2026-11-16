#![allow(clippy::pedantic)]

mod support;

use std::fs;

use oxiroute_epoll_h1::{Config, Splice, Timeouts};
use support::{Fixture, Knobs, Wire, sized, test_config};

const FDS_PER_PIPE: usize = 2;
const POOL: usize = 2;

fn open_pipes() -> usize {
    fs::read_dir("/proc/self/fd")
        .expect("fd table")
        .flatten()
        .filter(|entry| {
            fs::read_link(entry.path())
                .is_ok_and(|target| target.to_string_lossy().starts_with("pipe:"))
        })
        .count()
}

fn fetch(wire: &mut Wire, path: &str) -> Vec<u8> {
    wire.get(path);
    wire.reply().body
}

#[test]
fn pipes_open_only_for_large_bodies_and_never_exceed_the_pool_bound() {
    let config = Config {
        splice: Some(Splice {
            threshold: 32 * 1024,
            pipes: POOL,
            pipe_bytes: 64 * 1024,
        }),
        ..test_config()
    };
    let fixture = Fixture::with(config, Timeouts::default(), Knobs::default());
    let before = open_pipes();
    let mut wire = fixture.wire();
    for len in [1, 1000, 16_000, 30_000] {
        assert!(fetch(&mut wire, &format!("/size/{len}")) == sized(len));
    }
    assert_eq!(open_pipes(), before);
    assert!(fetch(&mut wire, "/size/500000") == sized(500_000));
    assert_eq!(open_pipes(), before + FDS_PER_PIPE);
    let mut wires: Vec<Wire> = (0..6).map(|_| fixture.wire()).collect();
    wires.iter_mut().for_each(|wire| wire.get("/size/1000000"));
    for wire in &mut wires {
        assert!(wire.reply().body == sized(1_000_000));
    }
    assert!(open_pipes() <= before + POOL * FDS_PER_PIPE);
}
