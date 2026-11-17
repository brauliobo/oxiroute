use std::net::TcpStream;

use crate::support::{Harness, client, read_all, shrink_send_buffer, small, write_all};
use oxiroute_uring::{Config, Event, Handle, Peek, Read, Ring, Sent};

const FROM: u64 = 11;
const TO: u64 = 12;

struct Relay {
    harness: Harness,
    from: Handle,
    to: Handle,
    upstream: Option<TcpStream>,
    downstream: Option<TcpStream>,
}

fn relay(config: Config) -> Relay {
    let mut harness = Harness::new(config);
    let (_listener, address) = harness.listener();
    let upstream = client(address);
    let (from, _) = harness.accept(FROM);
    let downstream = client(address);
    let (to, _) = harness.accept(TO);
    Relay {
        harness,
        from,
        to,
        upstream: Some(upstream),
        downstream: Some(downstream),
    }
}

fn unread(ring: &Ring, handle: Handle) -> Peek<usize> {
    ring.peek(handle, <[u8]>::len)
}

impl Relay {
    fn wait_for_bytes(&mut self) -> usize {
        loop {
            match unread(&self.harness.ring, self.from) {
                Peek::Held(count) => return count,
                Peek::Block => {
                    self.harness.until("received bytes", |event| {
                        matches!(event, Event::Readable { tag: FROM })
                    });
                }
                other => panic!("no bytes to forward: {other:?}"),
            }
        }
    }

    fn forward_all(&mut self, head: &[u8], now: bool) -> usize {
        let count = self.wait_for_bytes();
        loop {
            match self
                .harness
                .ring
                .forward((self.from, self.to), head, count, now)
                .sent
            {
                Sent::Queued(sent) => return sent,
                Sent::Busy => {
                    self.harness.until("writable", |event| {
                        matches!(event, Event::Writable { tag: TO })
                    });
                }
                Sent::Failed => panic!("forward failed"),
            }
        }
    }

    fn relay_body(&mut self, length: usize) {
        let mut moved = 0;
        while moved < length {
            moved += self.forward_all(b"", false);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        self.harness.ring.close(self.to);
        self.harness.ring.close(self.from);
        let downstream = self.downstream.take().expect("the downstream peer");
        let reader = std::thread::spawn(move || read_all(&downstream));
        self.harness.settle();
        reader.join().expect("the downstream peer reads to the end")
    }
}

fn pattern(length: usize) -> Vec<u8> {
    (0..length).map(|n| (n * 7 % 251) as u8).collect()
}

#[test]
fn a_body_larger_than_the_receive_buffers_is_relayed_in_order_without_exhausting_the_pool() {
    let config = Config {
        buffers: 4,
        buffer_size: 4096,
        ..small()
    };
    let mut relay = relay(config);
    let body = pattern(2 * 1024 * 1024);
    let upstream = relay.upstream.take().expect("the upstream peer");
    let writer = {
        let body = body.clone();
        std::thread::spawn(move || write_all(&upstream, &body))
    };
    relay.relay_body(body.len());
    writer.join().expect("the origin writes the body");
    assert!(relay.finish() == body);
}

#[test]
fn the_head_precedes_the_forwarded_bytes() {
    let mut relay = relay(small());
    write_all(relay.upstream.as_ref().expect("upstream"), b"body bytes");
    assert_eq!(relay.forward_all(b"HEAD:", false), 15);
    assert_eq!(relay.finish(), b"HEAD:body bytes");
}

#[test]
fn only_the_requested_count_is_forwarded_and_the_rest_stays_readable_in_order() {
    let mut relay = relay(small());
    write_all(relay.upstream.as_ref().expect("upstream"), b"0123456789");
    assert_eq!(relay.wait_for_bytes(), 10);
    assert_eq!(
        relay
            .harness
            .ring
            .forward((relay.from, relay.to), b"", 4, false)
            .sent,
        Sent::Queued(4)
    );
    assert_eq!(unread(&relay.harness.ring, relay.from), Peek::Held(6));
    let mut rest = [0_u8; 16];
    assert_eq!(
        relay.harness.ring.read(relay.from, &mut rest),
        Read::Data(6)
    );
    assert_eq!(&rest[..6], b"456789");
    assert_eq!(relay.finish(), b"0123");
}

#[test]
fn a_forward_reports_how_many_received_bytes_are_left() {
    let mut relay = relay(small());
    write_all(relay.upstream.as_ref().expect("upstream"), b"0123456789");
    assert_eq!(relay.wait_for_bytes(), 10);
    let pair = (relay.from, relay.to);
    assert_eq!(relay.harness.ring.forward(pair, b"", 4, false).left, 6);
    let rest = loop {
        let forwarded = relay.harness.ring.forward(pair, b"", 6, false);
        if forwarded.sent != Sent::Busy {
            break forwarded;
        }
        relay.harness.until("writable", |event| {
            matches!(event, Event::Writable { tag: TO })
        });
    };
    assert_eq!((rest.sent, rest.left), (Sent::Queued(6), 0));
    assert_eq!(relay.finish(), b"0123456789");
}

#[test]
fn skipping_bytes_leaves_the_rest_readable_and_the_next_receive_armed_once_all_are_gone() {
    let mut relay = relay(small());
    write_all(relay.upstream.as_ref().expect("upstream"), b"HEADbody");
    assert_eq!(relay.wait_for_bytes(), 8);
    assert_eq!(relay.harness.ring.skip(relay.from, 4), 4);
    assert_eq!(unread(&relay.harness.ring, relay.from), Peek::Held(4));
    assert_eq!(relay.harness.ring.skip(relay.from, 4), 0);
    assert_eq!(unread(&relay.harness.ring, relay.from), Peek::Block);
    write_all(relay.upstream.as_ref().expect("upstream"), b"more");
    assert_eq!(relay.wait_for_bytes(), 4);
    assert_eq!(relay.forward_all(b"", false), 4);
    assert_eq!(relay.finish(), b"more");
}

#[test]
fn a_forward_to_a_busy_socket_changes_nothing_and_goes_through_once_the_send_ends() {
    let config = Config {
        send_limit: 8 * 1024 * 1024,
        ..small()
    };
    let mut relay = relay(config);
    shrink_send_buffer(relay.downstream.as_ref().expect("downstream"));
    let first = vec![9_u8; 4 * 1024 * 1024];
    assert_eq!(
        relay.harness.ring.send(relay.to, &first, b""),
        Sent::Queued(first.len())
    );
    write_all(relay.upstream.as_ref().expect("upstream"), b"second");
    let count = relay.wait_for_bytes();
    assert_eq!(
        relay
            .harness
            .ring
            .forward((relay.from, relay.to), b"", count, false)
            .sent,
        Sent::Busy
    );
    assert_eq!(unread(&relay.harness.ring, relay.from), Peek::Held(6));
    let downstream = relay.downstream.take().expect("downstream");
    let reader = std::thread::spawn(move || read_all(&downstream));
    relay.harness.until("writable", |event| {
        matches!(event, Event::Writable { tag: TO })
    });
    assert_eq!(relay.forward_all(b"", false), 6);
    relay.harness.ring.close(relay.to);
    relay.harness.ring.close(relay.from);
    relay.harness.settle();
    let received = reader.join().expect("the peer reads to the end");
    assert_eq!(received.len(), first.len() + 6);
    assert_eq!(&received[first.len()..], b"second");
}

#[test]
fn a_slow_reader_gets_every_forwarded_byte_while_the_receive_buffers_wait_for_the_send() {
    let config = Config {
        buffers: 4,
        buffer_size: 4096,
        ..small()
    };
    let mut relay = relay(config);
    shrink_send_buffer(relay.downstream.as_ref().expect("downstream"));
    let body = pattern(1024 * 1024);
    let upstream = relay.upstream.take().expect("upstream");
    let writer = {
        let body = body.clone();
        std::thread::spawn(move || write_all(&upstream, &body))
    };
    let downstream = relay.downstream.take().expect("downstream");
    let reader = std::thread::spawn(move || {
        let mut received = Vec::new();
        let mut chunk = [0_u8; 1500];
        while received.len() < 1024 * 1024 {
            let count = std::io::Read::read(&mut &downstream, &mut chunk).expect("read");
            received.extend_from_slice(&chunk[..count]);
            std::thread::sleep(std::time::Duration::from_micros(50));
        }
        received
    });
    relay.relay_body(body.len());
    writer.join().expect("the origin writes the body");
    relay.harness.ring.close(relay.to);
    relay.harness.ring.close(relay.from);
    relay.harness.settle();
    assert!(reader.join().expect("the slow reader") == body);
}

#[test]
fn the_origin_closing_mid_body_is_reported_after_the_bytes_it_sent_were_forwarded() {
    let mut relay = relay(small());
    write_all(relay.upstream.as_ref().expect("upstream"), b"partial body");
    assert_eq!(relay.forward_all(b"", false), 12);
    drop(relay.upstream.take());
    relay.harness.until("end of file", |event| {
        matches!(event, Event::Eof { tag: FROM })
    });
    assert_eq!(unread(&relay.harness.ring, relay.from), Peek::Eof);
    assert_eq!(relay.finish(), b"partial body");
}

#[test]
fn an_immediate_forward_followed_by_a_close_reaches_the_peer_whole() {
    let config = Config {
        buffers: 4,
        buffer_size: 64 * 1024,
        ..small()
    };
    let mut relay = relay(config);
    shrink_send_buffer(relay.downstream.as_ref().expect("downstream"));
    let body = pattern(64 * 1024);
    write_all(relay.upstream.as_ref().expect("upstream"), &body);
    let mut moved = 0;
    let mut head: &[u8] = b"HEAD";
    while moved < body.len() {
        let sent = relay.forward_all(head, true);
        moved += sent - head.len();
        head = b"";
    }
    assert_eq!(relay.finish(), [b"HEAD".as_slice(), &body].concat());
}

#[test]
fn bytes_the_pool_had_to_move_to_the_heap_are_forwarded_like_the_others() {
    let mut harness = Harness::new(Config {
        buffers: 8,
        ..small()
    });
    let (_listener, address) = harness.listener();
    let mut sources = Vec::new();
    for tag in 100..108_u64 {
        let upstream = client(address);
        let (from, _) = harness.accept(tag);
        write_all(&upstream, format!("payload {tag}").as_bytes());
        sources.push((tag, from, upstream));
    }
    let sink = client(address);
    let (to, _) = harness.accept(TO);
    for (tag, from, _) in &sources {
        harness.readable(*tag);
        let Peek::Held(count) = harness.ring.peek(*from, <[u8]>::len) else {
            panic!("bytes were received");
        };
        let expected = format!("payload {tag}");
        assert_eq!(count, expected.len());
        loop {
            match harness.ring.forward((*from, to), b"", count, false).sent {
                Sent::Queued(_) => break,
                Sent::Busy => {
                    harness.until("writable", |event| {
                        matches!(event, Event::Writable { tag: TO })
                    });
                }
                Sent::Failed => panic!("forward failed"),
            }
        }
    }
    harness.ring.close(to);
    for (_, from, _) in &sources {
        harness.ring.close(*from);
    }
    harness.settle();
    let received = String::from_utf8(read_all(&sink)).expect("text");
    let expected: String = (100..108).map(|tag| format!("payload {tag}")).collect();
    assert_eq!(received, expected);
}
