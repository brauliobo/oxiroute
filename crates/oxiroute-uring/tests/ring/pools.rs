use std::net::TcpStream;

use crate::support::{Harness, client, small, write_all};
use oxiroute_uring::{Config, Handle, Peek, Read};

const SMALL: u32 = 4096;
const LARGE: u32 = 64 * 1024;

fn pooled(buffers: u16, large_buffers: u16) -> Config {
    Config {
        buffers,
        buffer_size: SMALL,
        large_buffers,
        large_buffer_size: LARGE,
        ..small()
    }
}

fn unread(harness: &Harness, handle: Handle) -> Peek<usize> {
    harness.ring.peek(handle, <[u8]>::len)
}

fn drain(harness: &mut Harness, handle: Handle, tag: u64, total: usize) -> Vec<usize> {
    let mut sizes = Vec::new();
    let mut taken = 0;
    let mut scratch = vec![0_u8; LARGE as usize];
    while taken < total {
        match unread(harness, handle) {
            Peek::Held(len) => {
                sizes.push(len);
                match harness.ring.read(handle, &mut scratch) {
                    Read::Data(count) => taken += count,
                    other => panic!("read ended early: {other:?}"),
                }
            }
            Peek::Block => harness.readable(tag),
            other => panic!("receive ended early: {other:?}"),
        }
    }
    sizes
}

#[test]
fn a_socket_that_fills_the_small_buffers_receives_in_the_large_ones_and_stays_correct_when_it_calms_down()
 {
    let mut harness = Harness::new(pooled(16, 4));
    let (_listener, address) = harness.listener();
    let peer = client(address);
    let (handle, _) = harness.accept(1);
    let payload = vec![7_u8; 1024 * 1024];
    let writer = {
        let stream: TcpStream = peer.try_clone().expect("clone");
        let payload = payload.clone();
        std::thread::spawn(move || write_all(&stream, &payload))
    };
    let sizes = drain(&mut harness, handle, 1, payload.len());
    writer.join().expect("the peer writes the payload");
    assert!(sizes.iter().any(|len| *len > SMALL as usize), "{sizes:?}");
    assert!(sizes.iter().all(|len| *len <= LARGE as usize), "{sizes:?}");
    write_all(&peer, b"calm");
    assert_eq!(drain(&mut harness, handle, 1, 4), [4]);
    write_all(&peer, b"calm again");
    assert_eq!(drain(&mut harness, handle, 1, 10), [10]);
}

#[test]
fn a_small_pool_of_large_buffers_serves_many_streaming_sockets_through_exhaustion() {
    let mut harness = Harness::new(pooled(8, 2));
    let (_listener, address) = harness.listener();
    let peers: Vec<(u64, Handle, TcpStream)> = (10..26_u64)
        .map(|tag| {
            let peer = client(address);
            let (handle, _) = harness.accept(tag);
            (tag, handle, peer)
        })
        .collect();
    let payload = vec![3_u8; 256 * 1024];
    let writers: Vec<_> = peers
        .iter()
        .map(|(_, _, peer)| {
            let stream = peer.try_clone().expect("clone");
            let payload = payload.clone();
            std::thread::spawn(move || write_all(&stream, &payload))
        })
        .collect();
    let mut taken = vec![0_usize; peers.len()];
    let mut scratch = vec![0_u8; LARGE as usize];
    while taken.iter().any(|count| *count < payload.len()) {
        harness.pump(20);
        for (at, (_, handle, _)) in peers.iter().enumerate() {
            while let Read::Data(count) = harness.ring.read(*handle, &mut scratch) {
                taken[at] += count;
            }
        }
    }
    for writer in writers {
        writer.join().expect("the peer writes the payload");
    }
    assert!(taken.iter().all(|count| *count == payload.len()));
}
