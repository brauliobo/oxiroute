//! Reading what a proxy answers without a short clock deciding that an answer is over. A request that was sent whole
//! (its head and the body its framing announces) gets an answer or a close, so the wait for the first byte is long; an
//! answer whose framing says that bytes are missing is awaited the same way. Only silence after a complete answer (or
//! after one that has no framing, such as a close delimited body) is measured with the short `QUIET` window, so a
//! loaded machine slows the test down instead of splitting an answer between two steps of a script.

use std::time::Duration;

use tokio::{io::AsyncReadExt as _, net::TcpStream, time::timeout};

use crate::h1_wire::find;

/// Silence that ends a complete answer, and the wait for an answer to a request that is not whole.
const QUIET: Duration = Duration::from_millis(300);
/// The longest a proxy gets to start or to finish an answer.
const ANSWER: Duration = Duration::from_secs(5);

/// What the bytes of a message are: a request, or a response (to a `HEAD` request or not).
#[derive(Clone, Copy)]
enum Messages {
    Requests,
    Responses { head_only: bool },
}

/// Appends what `stream` sends in answer to the bytes written since the last answer (`unanswered`) to `bytes` and says
/// whether the connection closed.
pub async fn read_step(stream: &mut TcpStream, unanswered: &[u8], bytes: &mut Vec<u8>) -> bool {
    let start = bytes.len();
    let sent_whole =
        find(unanswered, b"\r\n\r\n").is_some() && whole(unanswered, Messages::Requests);
    let responses = Messages::Responses {
        head_only: unanswered.starts_with(b"HEAD "),
    };
    let mut buffer = vec![0_u8; 8192];
    loop {
        let received = &bytes[start..];
        let wait = match (received.is_empty(), sent_whole) {
            (true, true) => ANSWER,
            (false, _) if !whole(received, responses) => ANSWER,
            _ => QUIET,
        };
        match timeout(wait, stream.read(&mut buffer)).await {
            Ok(Ok(0) | Err(_)) => return true,
            Ok(Ok(count)) => bytes.extend_from_slice(&buffer[..count]),
            Err(_) => return false,
        }
    }
}

/// Whether `bytes` holds whole messages, or ends in one whose framing cannot be told: a head that is not complete yet,
/// or a response body without a length, which ends with the connection.
fn whole(mut bytes: &[u8], messages: Messages) -> bool {
    while !bytes.is_empty() {
        let Some(end) = find(bytes, b"\r\n\r\n").map(|at| at + 4) else {
            return true;
        };
        let head = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
        bytes = &bytes[end..];
        if let Messages::Responses { head_only } = messages {
            let status = head
                .split(' ')
                .nth(1)
                .and_then(|status| status.parse::<u16>().ok());
            if head_only
                || status.is_none_or(|status| status < 200 || status == 204 || status == 304)
            {
                continue;
            }
        }
        if head.contains("\r\ntransfer-encoding: chunked\r\n") {
            match chunked_len(bytes) {
                Some(length) => bytes = &bytes[length..],
                None => return false,
            }
        } else if let Some(length) = content_length(&head) {
            if bytes.len() < length {
                return false;
            }
            bytes = &bytes[length..];
        } else if matches!(messages, Messages::Responses { .. }) {
            return true;
        }
    }
    true
}

fn content_length(head: &str) -> Option<usize> {
    head.lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse().ok())
}

/// The length of the chunked body at the start of `body` with its last chunk, or `None` while chunks are missing. A
/// size that is not a number ends the body there: the proxies are compared on what they send, not on what they accept.
fn chunked_len(body: &[u8]) -> Option<usize> {
    let mut at = 0;
    loop {
        let line_end = at + find(&body[at..], b"\r\n")?;
        let line = String::from_utf8_lossy(&body[at..line_end]);
        let Ok(size) = usize::from_str_radix(line.split(';').next().unwrap_or_default().trim(), 16)
        else {
            return Some(body.len());
        };
        at = line_end + 2;
        if size == 0 {
            return (body.len() >= at + 2).then_some(at + 2);
        }
        at += size + 2;
        if at > body.len() {
            return None;
        }
    }
}
