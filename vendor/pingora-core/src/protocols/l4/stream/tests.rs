// Copyright 2026 Cloudflare, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use super::*;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::Notify;

#[cfg(target_os = "linux")]
#[tokio::test]
async fn test_rx_timestamp() {
    let message = "hello world".as_bytes();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let notify = Arc::new(Notify::new());
    let notify2 = notify.clone();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        notify2.notified().await;
        stream.write_all(message).await.unwrap();
    });

    let mut stream: Stream = TcpStream::connect(addr).await.unwrap().into();
    stream.set_rx_timestamp().unwrap();
    // Receive the message
    // setsockopt for SO_TIMESTAMPING is asynchronous so sleep a little bit
    // to let kernel do the work
    std::thread::sleep(Duration::from_micros(100));
    notify.notify_one();

    let mut buffer = vec![0u8; message.len()];
    let n = stream.read(buffer.as_mut_slice()).await.unwrap();
    assert_eq!(n, message.len());
    assert!(stream.rx_ts.is_some());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn test_rx_timestamp_standard_path() {
    let message = "hello world".as_bytes();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let notify = Arc::new(Notify::new());
    let notify2 = notify.clone();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        notify2.notified().await;
        stream.write_all(message).await.unwrap();
    });

    let mut stream: Stream = TcpStream::connect(addr).await.unwrap().into();
    std::thread::sleep(Duration::from_micros(100));
    notify.notify_one();

    let mut buffer = vec![0u8; message.len()];
    let n = stream.read(buffer.as_mut_slice()).await.unwrap();
    assert_eq!(n, message.len());
    assert!(stream.rx_ts.is_none());
}

#[tokio::test]
async fn test_stream_rewind() {
    let message = b"hello world";
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let notify = Arc::new(Notify::new());
    let notify2 = notify.clone();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        notify2.notified().await;
        stream.write_all(message).await.unwrap();
    });

    let mut stream: Stream = TcpStream::connect(addr).await.unwrap().into();

    let rewind_test = b"this is Sparta!";
    stream.rewind(rewind_test);

    // partially read rewind_test because of the buffer size limit
    let mut buffer = vec![0u8; message.len()];
    let n = stream.read(buffer.as_mut_slice()).await.unwrap();
    assert_eq!(n, message.len());
    assert_eq!(buffer, rewind_test[..message.len()]);

    // read the rest of rewind_test
    let n = stream.read(buffer.as_mut_slice()).await.unwrap();
    assert_eq!(n, rewind_test.len() - message.len());
    assert_eq!(buffer[..n], rewind_test[message.len()..]);

    // read the actual data
    notify.notify_one();
    let n = stream.read(buffer.as_mut_slice()).await.unwrap();
    assert_eq!(n, message.len());
    assert_eq!(buffer, message);
}

#[tokio::test]
async fn test_stream_peek() {
    let message = b"hello world";
    dbg!("try peek");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let notify = Arc::new(Notify::new());
    let notify2 = notify.clone();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        notify2.notified().await;
        stream.write_all(message).await.unwrap();
        drop(stream);
    });

    notify.notify_one();

    let mut stream: Stream = TcpStream::connect(addr).await.unwrap().into();
    let mut buffer = vec![0u8; 5];
    assert!(stream.try_peek(&mut buffer).await.unwrap());
    assert_eq!(buffer, message[0..5]);
    let mut buffer = vec![];
    stream.read_to_end(&mut buffer).await.unwrap();
    assert_eq!(buffer, message);
}

#[tokio::test]
async fn test_stream_two_subsequent_peek_calls_before_read() {
    let message = b"abcdefghijklmn";

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let notify = Arc::new(Notify::new());
    let notify2 = notify.clone();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        notify2.notified().await;
        stream.write_all(message).await.unwrap();
        drop(stream);
    });

    notify.notify_one();

    let mut stream: Stream = TcpStream::connect(addr).await.unwrap().into();

    // Peek 4 bytes
    let mut buffer = vec![0u8; 4];
    assert!(stream.try_peek(&mut buffer).await.unwrap());
    assert_eq!(buffer, message[0..4]);

    // Peek 2 bytes
    let mut buffer = vec![0u8; 2];
    assert!(stream.try_peek(&mut buffer).await.unwrap());
    assert_eq!(buffer, message[0..2]);

    // Read 1 byte: ['a']
    let mut buffer = vec![0u8; 1];
    stream.read_exact(&mut buffer).await.unwrap();
    assert_eq!(buffer, message[0..1]);

    // Read as many bytes as possible, return 1 byte ['b']
    //  from the first retry buffer chunk
    let mut buffer = vec![0u8; 100];
    let n = stream.read(&mut buffer).await.unwrap();
    assert_eq!(n, 1);
    assert_eq!(buffer[..n], message[1..2]);

    // Read the rest ['cdefghijklmn']
    let mut buffer = vec![];
    stream.read_to_end(&mut buffer).await.unwrap();
    assert_eq!(buffer, message[2..]);
}

#[tokio::test]
async fn read_ready_follows_the_socket_without_reading_it() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut peer = TcpStream::connect(addr).await.unwrap();
    let mut stream: Stream = listener.accept().await.unwrap().0.into();

    // A read that finds nothing is what tells the runtime the socket is not readable.
    let mut buf = [0; 5];
    assert!(futures::poll!(std::pin::pin!(stream.read(&mut buf))).is_pending());
    assert!(futures::poll!(std::future::poll_fn(|cx| stream.poll_read_ready(cx))).is_pending());

    peer.write_all(b"ready").await.unwrap();
    std::future::poll_fn(|cx| stream.poll_read_ready(cx))
        .await
        .unwrap();
    // Asking does not consume anything.
    stream.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"ready");
}

#[tokio::test]
async fn test_buffer_state_of_a_tcp_stream_is_visible_for_kernel_offload() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut peer, _) = listener.accept().await.unwrap();
        peer.write_all(b"abcdef").await.unwrap();
        let mut sink = Vec::new();
        peer.read_to_end(&mut sink).await.unwrap();
    });

    let mut stream: Stream = TcpStream::connect(addr).await.unwrap().into();
    stream
        .set_buffer(L4BufferSettings::default(), false)
        .unwrap();
    assert!(stream.tcp_socket().is_some());
    assert!(!stream.has_read_ahead() && !stream.has_write_behind());

    let mut head = [0u8; 2];
    stream.read_exact(&mut head).await.unwrap();
    assert!(
        stream.has_read_ahead(),
        "four bytes were read from the socket and not consumed"
    );
    let mut rest = [0u8; 4];
    stream.read_exact(&mut rest).await.unwrap();
    assert!(!stream.has_read_ahead());

    stream.write_all(b"ab").await.unwrap();
    assert!(
        stream.has_write_behind(),
        "a short write waits in the write buffer"
    );
    stream.flush().await.unwrap();
    assert!(!stream.has_write_behind());

    stream.rewind(b"x");
    assert!(stream.has_read_ahead());
}

#[tokio::test]
async fn test_a_unix_stream_has_no_tcp_socket() {
    let (near, _far) = tokio::net::UnixStream::pair().unwrap();
    let stream: Stream = near.into();
    assert!(stream.tcp_socket().is_none());
}

#[tokio::test]
async fn try_read_now_takes_what_the_socket_holds_without_the_driver() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut client = TcpStream::connect(addr).await.unwrap();
    let (accepted, _) = listener.accept().await.unwrap();
    let mut stream: Stream = accepted.into();
    let mut buffer = [0u8; 16];

    // nothing yet: no bytes and no waiting
    assert_eq!(stream.try_read_now(&mut buffer).unwrap(), None);

    // the socket has no readiness event for the bytes yet; they are read all the same
    client.write_all(b"hello").await.unwrap();
    for _ in 0..100 {
        if let Some(count) = stream.try_read_now(&mut buffer).unwrap() {
            assert_eq!(&buffer[..count], b"hello");
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(stream.try_read_now(&mut buffer).unwrap(), None);

    // the stream reads on as usual afterwards
    client.write_all(b"world").await.unwrap();
    let mut rest = [0u8; 5];
    stream.read_exact(&mut rest).await.unwrap();
    assert_eq!(&rest, b"world");

    // the end of the stream is a zero-length read
    drop(client);
    for _ in 0..100 {
        if let Some(count) = stream.try_read_now(&mut buffer).unwrap() {
            assert_eq!(count, 0);
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the close was not seen");
}

#[tokio::test]
async fn try_read_now_leaves_bytes_put_back_to_the_reader() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut client = TcpStream::connect(addr).await.unwrap();
    let (accepted, _) = listener.accept().await.unwrap();
    let mut stream: Stream = accepted.into();
    stream.rewind(b"early");
    client.write_all(b"later").await.unwrap();
    let mut buffer = [0u8; 16];

    assert_eq!(stream.try_read_now(&mut buffer).unwrap(), None);
    let mut head = [0u8; 5];
    stream.read_exact(&mut head).await.unwrap();
    assert_eq!(&head, b"early");
}

#[tokio::test]
async fn try_read_now_is_not_offered_by_a_unix_stream() {
    let (left, _right) = tokio::net::UnixStream::pair().unwrap();
    let mut stream: Stream = left.into();
    assert_eq!(stream.try_read_now(&mut [0u8; 4]).unwrap(), None);
}
