//! Reading what a proxy answers without a short clock deciding that an answer is over. A request that was sent whole
//! (its head and the body its framing announces) gets as many answers as it holds requests, each complete when its
//! framing says so, or a close, so a loaded machine slows the test down instead of splitting an answer between two
//! steps of a script. Silence is only waited out where nothing can announce the end: after the last step of a script,
//! which shows that the proxy sends nothing more and keeps the connection, after an answer that says the connection
//! closes (a proxy may close later than that or not at all, and the scripts record which), and after an answer that
//! ends with the connection.

use std::time::Duration;

use tokio::{io::AsyncReadExt as _, net::TcpStream, time::timeout};

use crate::h1_wire::find;

/// Silence that ends a script and an answer without framing.
const QUIET: Duration = Duration::from_millis(300);
/// The longest a proxy gets to start or to finish an answer.
const ANSWER: Duration = Duration::from_secs(5);

/// What the bytes a proxy sent in answer to a step say about the rest of the answer.
enum Answer {
    /// Whole answers, and whether the connection closes after the last of them.
    Whole { count: usize, closes: bool },
    /// Bytes of an answer are still to come.
    Partial,
    /// The last answer has no framing: it ends with the connection.
    Unframed,
}

impl Answer {
    /// How long to wait for more bytes of the answer to `requests` requests, or `None` when the step is over.
    fn wait(&self, requests: usize, last: bool) -> Option<Duration> {
        match self {
            Self::Unframed => Some(QUIET),
            Self::Whole { count, closes } if *count >= requests => {
                (*closes || last).then_some(QUIET)
            }
            _ => Some(ANSWER),
        }
    }
}

/// Appends what `stream` sends in answer to the bytes written since the last answer (`unanswered`) to `bytes` and says
/// whether the connection closed. `last` is whether the script ends with this step.
pub async fn read_step(
    stream: &mut TcpStream,
    unanswered: &[u8],
    bytes: &mut Vec<u8>,
    last: bool,
) -> bool {
    let start = bytes.len();
    let (requests, _) = requests_in(unanswered);
    let head_only = unanswered.starts_with(b"HEAD ");
    let mut buffer = vec![0_u8; 8192];
    while let Some(wait) = answer(&bytes[start..], head_only).wait(requests, last) {
        match timeout(wait, stream.read(&mut buffer)).await {
            Ok(Ok(0) | Err(_)) => return true,
            Ok(Ok(count)) => bytes.extend_from_slice(&buffer[..count]),
            Err(_) => return false,
        }
    }
    false
}

/// The requests `bytes` holds whole, and whether none of the messages after them lacks the body its framing announces.
fn requests_in(mut bytes: &[u8]) -> (usize, bool) {
    let mut count = 0;
    while let Some(end) = find(bytes, b"\r\n\r\n").map(|at| at + 4) {
        let head = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
        if head.trim().is_empty() {
            bytes = &bytes[end..];
            continue;
        }
        let body = if head.contains("\r\ntransfer-encoding: chunked\r\n") {
            chunked_len(&bytes[end..])
        } else {
            let length = content_length(&head).unwrap_or(0);
            (bytes.len() >= end + length).then_some(length)
        };
        let Some(body) = body else {
            return (count, false);
        };
        count += 1;
        bytes = &bytes[end + body..];
    }
    (count, true)
}

/// Whether an origin got `request` whole: its head and the body its framing announces. A request cut short is what a
/// proxy forwarded of a request it did not finish, and how much that is depends on its reads, not on its behaviour.
pub fn is_whole_request(request: &str) -> bool {
    let (count, complete) = requests_in(request.as_bytes());
    count > 0 && complete
}

/// What `bytes`, the answers of a proxy, hold.
fn answer(mut bytes: &[u8], head_only: bool) -> Answer {
    let mut count = 0;
    let mut closes = false;
    while !bytes.is_empty() {
        let Some(end) = find(bytes, b"\r\n\r\n").map(|at| at + 4) else {
            return Answer::Partial;
        };
        let head = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
        bytes = &bytes[end..];
        let status = head
            .split(' ')
            .nth(1)
            .and_then(|status| status.parse::<u16>().ok());
        let Some(status) = status else {
            return Answer::Unframed;
        };
        if status == 101 {
            return Answer::Unframed;
        }
        if status < 200 {
            continue;
        }
        closes = connection_closes(&head);
        count += 1;
        if head_only || status == 204 || status == 304 {
            continue;
        }
        if head.contains("\r\ntransfer-encoding: chunked\r\n") {
            match chunked_len(bytes) {
                Some(length) => bytes = &bytes[length..],
                None => return Answer::Partial,
            }
        } else if let Some(length) = content_length(&head) {
            if bytes.len() < length {
                return Answer::Partial;
            }
            bytes = &bytes[length..];
        } else {
            return Answer::Unframed;
        }
    }
    Answer::Whole { count, closes }
}

fn connection_closes(head: &str) -> bool {
    head.lines()
        .find_map(|line| line.strip_prefix("connection:"))
        .is_some_and(|value| value.split(',').any(|token| token.trim() == "close"))
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
