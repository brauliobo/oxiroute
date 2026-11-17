use super::reference_scan::{self as reference, Stop};
use super::{MUTATION_ALPHABET, Rng, mutate, request_corpus, response_corpus};
use oxiroute_h1_head::{
    RequestHead, ResponseHead, Scan, ScanResponse, scan_request, scan_request_fields,
    scan_request_line, scan_response,
};

const WHOLE_UP_TO: usize = 512;
const EDGE: usize = 96;
const STRIDE: usize = 509;

pub fn same_request(buf: &[u8], head: &mut RequestHead) {
    let shown = String::from_utf8_lossy(buf);
    let scan = scan_request(buf, head);
    match (reference::request(buf), scan) {
        (Ok(expected), Scan::Head) => {
            assert_eq!(head.method, expected.method, "method of {shown:?}");
            assert_eq!(head.target, expected.target, "target of {shown:?}");
            assert_eq!(head.line_len, expected.line_len, "line of {shown:?}");
            assert_eq!(head.head_len, expected.head_len, "head of {shown:?}");
            assert_eq!(head.headers(), expected.headers, "headers of {shown:?}");
            assert_eq!(head.facts, expected.facts, "facts of {shown:?}");
            assert_eq!(usize::from(head.count), expected.headers.len());
        }
        (Err(Stop::Partial), Scan::Partial) => {}
        (Err(Stop::Bad(expected)), Scan::Miss(reason)) => {
            assert_eq!(reason, expected, "reason for {shown:?}");
        }
        (expected, scan) => panic!("{shown:?}: specification {expected:?}, scanner {scan:?}"),
    }
}

pub fn same_split_request(buf: &[u8], head: &mut RequestHead) {
    let mut split = RequestHead::new();
    let whole = scan_request(buf, head);
    let line = scan_request_line(buf);
    match (line, whole) {
        (Ok(line), Scan::Head | Scan::Miss(_) | Scan::Partial) => {
            let fields = scan_request_fields(buf, line, &mut split);
            assert_eq!(fields, whole, "{:?}", String::from_utf8_lossy(buf));
            if fields == Scan::Head {
                assert_eq!(split.headers(), head.headers());
                assert_eq!(split.facts, head.facts);
                assert_eq!((split.method, split.target), (head.method, head.target));
            }
        }
        (Err(scan), whole) => assert_eq!(scan, whole, "{:?}", String::from_utf8_lossy(buf)),
    }
}

pub fn same_response(buf: &[u8], head: &mut ResponseHead) {
    let shown = String::from_utf8_lossy(buf);
    let scan = scan_response(buf, head);
    match (reference::response(buf), scan) {
        (Ok(expected), ScanResponse::Head) => {
            assert_eq!(head.status, expected.status, "status of {shown:?}");
            assert_eq!(head.minor, expected.minor, "minor of {shown:?}");
            assert_eq!(head.reason, expected.reason, "reason of {shown:?}");
            assert_eq!(head.line_len, expected.line_len, "line of {shown:?}");
            assert_eq!(head.head_len, expected.head_len, "head of {shown:?}");
            assert_eq!(head.headers(), expected.headers, "headers of {shown:?}");
            assert_eq!(head.facts, expected.facts, "facts of {shown:?}");
        }
        (Err(Stop::Partial), ScanResponse::Partial) | (Err(Stop::Bad(_)), ScanResponse::Slow) => {}
        (expected, scan) => panic!("{shown:?}: specification {expected:?}, scanner {scan:?}"),
    }
}

fn request_inputs() -> Vec<Vec<u8>> {
    request_corpus::corpus()
        .into_iter()
        .map(|(_, bytes, _)| bytes)
        .collect()
}

fn response_inputs() -> Vec<Vec<u8>> {
    response_corpus::corpus()
        .into_iter()
        .map(|(_, bytes, _)| bytes)
        .collect()
}

/// Every cut of a short input; of a long one, the cuts around both ends and one in every `STRIDE` bytes, which still
/// cross every vector step and alignment while the work stays linear in its length.
fn cuts(len: usize) -> impl Iterator<Item = usize> {
    let sparse = len > WHOLE_UP_TO;
    (0..=len).filter(move |&cut| !sparse || cut < EDGE || cut + EDGE >= len || cut % STRIDE == 0)
}

pub fn request_corpus_prefixes() {
    let (mut head, mut other) = (RequestHead::new(), RequestHead::new());
    for bytes in request_inputs() {
        for cut in cuts(bytes.len()) {
            same_request(&bytes[..cut], &mut head);
            same_split_request(&bytes[..cut], &mut other);
        }
    }
}

pub fn response_corpus_prefixes() {
    let mut head = ResponseHead::new();
    for bytes in response_inputs() {
        for cut in cuts(bytes.len()) {
            same_response(&bytes[..cut], &mut head);
        }
    }
}

pub fn mutated_corpora(rounds: usize) {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let (mut request, mut response) = (RequestHead::new(), ResponseHead::new());
    let (requests, responses) = (request_inputs(), response_inputs());
    for round in 0..rounds {
        let mut bytes = rng.pick(&requests).clone();
        mutate(&mut rng, &mut bytes, MUTATION_ALPHABET);
        same_request(&bytes, &mut request);
        let mut bytes = rng.pick(&responses).clone();
        mutate(&mut rng, &mut bytes, MUTATION_ALPHABET);
        same_response(&bytes, &mut response);
        if round % 16 == 0 {
            let cut = rng.below(bytes.len() + 1);
            same_response(&bytes[..cut], &mut response);
        }
    }
}

const NAME_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_.";
const VALUE_ALPHABET: &[u8] = b"abcdef ZZ09-_.,;:=/\"\t \x80\xfe";

fn text(rng: &mut Rng, alphabet: &[u8], len: usize) -> Vec<u8> {
    (0..len).map(|_| *rng.pick(alphabet)).collect()
}

fn length(rng: &mut Rng) -> usize {
    *rng.pick(&[
        1, 2, 3, 7, 8, 15, 16, 17, 30, 31, 32, 33, 34, 63, 64, 65, 100, 256, 300,
    ])
}

fn text_of_any_length(rng: &mut Rng, alphabet: &[u8], empty_one_in: usize) -> Vec<u8> {
    let len = length(rng) * usize::from(!rng.chance(empty_one_in));
    text(rng, alphabet, len)
}

fn generated_fields(rng: &mut Rng, with_host: bool) -> Vec<u8> {
    let mut fields = Vec::new();
    if with_host {
        fields.extend_from_slice(b"Host: example.com\r\n");
    }
    for _ in 0..rng.below(12) {
        let name_len = if rng.chance(8) {
            length(rng)
        } else {
            1 + rng.below(20)
        };
        fields.extend(text(rng, NAME_ALPHABET, name_len));
        fields.extend_from_slice(if rng.chance(4) { b":" } else { b": " });
        fields.extend(text_of_any_length(rng, VALUE_ALPHABET, 6));
        fields.extend_from_slice(b"\r\n");
    }
    fields.extend_from_slice(b"\r\n");
    fields
}

pub fn generated_requests(rounds: usize) {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut head = RequestHead::new();
    for _ in 0..rounds {
        let mut bytes = b"GET /".to_vec();
        bytes.extend(text_of_any_length(&mut rng, b"abc/?=&%1-_.~", 4));
        bytes.extend_from_slice(b" HTTP/1.1\r\n");
        let with_host = !rng.chance(10);
        bytes.extend(generated_fields(&mut rng, with_host));
        same_request(&bytes, &mut head);
        if rng.chance(8) {
            mutate(&mut rng, &mut bytes, MUTATION_ALPHABET);
            same_request(&bytes, &mut head);
        }
        let cut = rng.below(bytes.len() + 1);
        same_request(&bytes[..cut], &mut head);
    }
}

pub fn generated_responses(rounds: usize) {
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let mut head = ResponseHead::new();
    for _ in 0..rounds {
        let mut bytes = b"HTTP/1.1 200 ".to_vec();
        bytes.extend(text_of_any_length(&mut rng, b"OK ok\t", 3));
        bytes.extend_from_slice(b"\r\n");
        bytes.extend(generated_fields(&mut rng, false));
        same_response(&bytes, &mut head);
        if rng.chance(8) {
            mutate(&mut rng, &mut bytes, MUTATION_ALPHABET);
            same_response(&bytes, &mut head);
        }
        let cut = rng.below(bytes.len() + 1);
        same_response(&bytes[..cut], &mut head);
    }
}

pub fn limits_of_names_targets_methods_and_header_counts() {
    let mut head = RequestHead::new();
    let host = "Host: a\r\n";
    for name_len in [254, 255, 256, 257] {
        let name = "n".repeat(name_len);
        same_request(
            format!("GET / HTTP/1.1\r\n{host}{name}: v\r\n\r\n").as_bytes(),
            &mut head,
        );
    }
    for method_len in [31, 32, 33, 34] {
        let method = "M".repeat(method_len);
        same_request(
            format!("{method} / HTTP/1.1\r\n{host}\r\n").as_bytes(),
            &mut head,
        );
    }
    for target_len in [30, 31, 32, 33, 64, 65, 65_533, 65_534, 65_535] {
        let target = "t".repeat(target_len);
        same_request(
            format!("GET /{target} HTTP/1.1\r\n{host}\r\n").as_bytes(),
            &mut head,
        );
    }
    for headers in [62, 63, 64, 65, 66] {
        let fields = (0..headers).fold(String::new(), |all, i| all + &format!("X-{i}: v\r\n"));
        same_request(
            format!("GET / HTTP/1.1\r\n{host}{fields}\r\n").as_bytes(),
            &mut head,
        );
    }
}
