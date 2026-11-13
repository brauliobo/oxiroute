//! Read and write buffers of the lane: an idle connection holds none. A buffer is taken from a per-thread pool when
//! a request starts to arrive and goes back when the request is served, so the buffers a thread needs are the ones
//! its busy connections use at the same time.

use std::{cell::RefCell, future::poll_fn, pin::Pin, task::Poll};

use bytes::BytesMut;
use oxiroute_h1_head::{RequestHead, ResponseHead};
use pingora::protocols::Stream;
use tokio::io::{AsyncRead, ReadBuf};

const READ_CAPACITY: usize = 8 * 1024;
/// What one read of an upload takes from the client: a request body larger than the head's read buffer is read in
/// pieces of at most this size.
const UPLOAD_CAPACITY: usize = 64 * 1024;
const UPLOADS_POOLED: usize = 16;
const POOLED: usize = 64;
const WRITE_CAPACITY: usize = 1024;

thread_local! {
    static READ: RefCell<Vec<BytesMut>> = const { RefCell::new(Vec::new()) };
    // The heads are large and move in and out of the pool by pointer.
    #[allow(clippy::vec_box)]
    static HEADS: RefCell<Vec<Box<RequestHead>>> = const { RefCell::new(Vec::new()) };
    #[allow(clippy::vec_box)]
    static RESPONSES: RefCell<Vec<Box<ResponseHead>>> = const { RefCell::new(Vec::new()) };
    static WRITE: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
    static UPLOADS: RefCell<Vec<BytesMut>> = const { RefCell::new(Vec::new()) };
    // Where the bytes of a request are read before a pooled buffer is taken for them: initialised once, so a read
    // that finds nothing costs no buffer.
    static ARRIVAL: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// The bytes a new connection holds already, read at once. The I/O driver has no readiness event for a connection that
/// was just accepted, so waiting for one parks the task although the request is usually in the socket. Nothing is
/// read when the socket is empty, the peer is gone or the transport cannot say; the wait that follows reports those.
pub(in crate::http::proxy) fn read_arrived(stream: &mut Stream) -> Option<BytesMut> {
    ARRIVAL.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        if scratch.is_empty() {
            scratch.resize(READ_CAPACITY, 0);
        }
        let Ok(Some(count @ 1..)) = stream.try_read_now(&mut scratch) else {
            return None;
        };
        Some(pooled_copy(&scratch[..count]))
    })
}

/// Waits for the stream's next bytes and returns them in a pooled buffer, `None` when the stream ended or failed. The
/// wait is on readiness, which costs no read of the transport (a TLS stream would try to decrypt), and the read goes into
/// the thread's scratch, so a connection that is waiting holds no buffer: a wait that is woken without bytes (readiness
/// the last read left behind) would otherwise leave an 8 KiB buffer taken for as long as it lasts.
pub(in crate::http::proxy) async fn read_request(stream: &mut Stream) -> Option<BytesMut> {
    poll_fn(|cx| {
        match stream.poll_read_ready(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(_)) => return Poll::Ready(None),
            Poll::Ready(Ok(())) => {}
        }
        ARRIVAL.with(|scratch| {
            let mut scratch = scratch.borrow_mut();
            if scratch.is_empty() {
                scratch.resize(READ_CAPACITY, 0);
            }
            let mut read = ReadBuf::new(&mut scratch);
            match Pin::new(&mut *stream).poll_read(cx, &mut read) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Ok(())) => Poll::Ready(
                    Some(read.filled())
                        .filter(|bytes| !bytes.is_empty())
                        .map(pooled_copy),
                ),
                Poll::Ready(Err(_)) => Poll::Ready(None),
            }
        })
    })
    .await
}

fn pooled_copy(bytes: &[u8]) -> BytesMut {
    let mut buffer = take_read();
    buffer.extend_from_slice(bytes);
    buffer
}

pub(in crate::http::proxy) fn take_read() -> BytesMut {
    READ.with(|pool| pool.borrow_mut().pop())
        .unwrap_or_else(|| BytesMut::with_capacity(READ_CAPACITY))
}

/// Keeps `buffer` for the next request of any connection of this thread; a buffer that grew is dropped.
pub(in crate::http::proxy) fn give_read(mut buffer: BytesMut) {
    if buffer.capacity() < READ_CAPACITY || buffer.capacity() > 2 * READ_CAPACITY {
        return;
    }
    buffer.clear();
    READ.with(|pool| {
        let mut pool = pool.borrow_mut();
        if pool.len() < POOLED {
            pool.push(buffer);
        }
    });
}

/// A buffer one read of an upload goes into.
pub(in crate::http::proxy) fn take_upload() -> BytesMut {
    UPLOADS
        .with(|pool| pool.borrow_mut().pop())
        .unwrap_or_else(|| BytesMut::with_capacity(UPLOAD_CAPACITY))
}

pub(in crate::http::proxy) fn give_upload(mut buffer: BytesMut) {
    if buffer.capacity() < UPLOAD_CAPACITY {
        return;
    }
    buffer.clear();
    UPLOADS.with(|pool| {
        let mut pool = pool.borrow_mut();
        if pool.len() < UPLOADS_POOLED {
            pool.push(buffer);
        }
    });
}

/// A scanned-head record, kept per thread because it is large and a request holds one only while it is served.
pub(in crate::http::proxy) fn take_head() -> Box<RequestHead> {
    HEADS
        .with(|pool| pool.borrow_mut().pop())
        .unwrap_or_else(|| Box::new(RequestHead::new()))
}

pub(in crate::http::proxy) fn give_head(head: Box<RequestHead>) {
    HEADS.with(|pool| {
        let mut pool = pool.borrow_mut();
        if pool.len() < POOLED {
            pool.push(head);
        }
    });
}

/// A buffer a head is rendered into.
pub(in crate::http::proxy) fn take_write() -> Vec<u8> {
    WRITE
        .with(|pool| pool.borrow_mut().pop())
        .unwrap_or_else(|| Vec::with_capacity(WRITE_CAPACITY))
}

pub(in crate::http::proxy) fn give_write(mut buffer: Vec<u8>) {
    if buffer.capacity() == 0 || buffer.capacity() > 4 * WRITE_CAPACITY {
        return;
    }
    buffer.clear();
    WRITE.with(|pool| {
        let mut pool = pool.borrow_mut();
        if pool.len() < POOLED {
            pool.push(buffer);
        }
    });
}

/// A scanned response head, kept per thread for the same reason.
pub(in crate::http::proxy) fn take_response_head() -> Box<ResponseHead> {
    RESPONSES
        .with(|pool| pool.borrow_mut().pop())
        .unwrap_or_else(|| Box::new(ResponseHead::new()))
}

pub(in crate::http::proxy) fn give_response_head(head: Box<ResponseHead>) {
    RESPONSES.with(|pool| {
        let mut pool = pool.borrow_mut();
        if pool.len() < POOLED {
            pool.push(head);
        }
    });
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::{
        io::AsyncWriteExt as _,
        net::{TcpListener, TcpStream},
        time::timeout,
    };

    use super::*;

    async fn connected() -> (TcpStream, Stream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let client = TcpStream::connect(listener.local_addr().expect("address"))
            .await
            .expect("connect");
        let (server, _) = listener.accept().await.expect("accept");
        let stream = pingora::protocols::l4::stream::Stream::from(server);
        (client, Box::new(stream))
    }

    fn pooled() -> usize {
        READ.with(|pool| pool.borrow().len())
    }

    #[tokio::test]
    async fn a_connection_waiting_for_its_next_request_holds_no_read_buffer() {
        let (mut client, mut stream) = connected().await;
        give_read(BytesMut::with_capacity(READ_CAPACITY));
        let request = b"GET / HTTP/1.1\r\nHost: idle.test\r\n\r\n";

        // The first request, then a wait that starts with the readiness of the first still set.
        client.write_all(request).await.expect("write");
        let first = read_request(&mut stream).await.expect("the first request");
        assert_eq!(&first[..], request);
        assert_eq!(
            pooled(),
            0,
            "the bytes of a request are read into a pooled buffer"
        );
        give_read(first);

        let waiting = timeout(Duration::from_millis(50), read_request(&mut stream)).await;
        assert!(waiting.is_err(), "nothing was sent");
        assert_eq!(
            pooled(),
            1,
            "a wait for bytes that have not come holds a buffer"
        );

        client.write_all(request).await.expect("write");
        let second = read_request(&mut stream).await.expect("the second request");
        assert_eq!(&second[..], request);

        drop(client);
        assert!(
            read_request(&mut stream).await.is_none(),
            "a closed stream ends the read"
        );
    }
}
