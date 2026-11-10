//! Differential tests against the `h2` crate over in-memory pipes: its client encodes header blocks that this
//! decoder must read back exactly, and its client decodes header blocks this encoder produced.
use bytes::Bytes;
use h2::client::{Builder, SendRequest};
use http::{HeaderValue, Request};
use oxiroute_h2::hpack::{Decoder, Encoder, Indexing};
use std::future::Future;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, duplex};

type Fields = Vec<(Vec<u8>, Vec<u8>)>;

const HEADERS: u8 = 1;
const SETTINGS: u8 = 4;
const CONTINUATION: u8 = 9;
const ACK: u8 = 1;
const END_STREAM: u8 = 1;
const END_HEADERS: u8 = 4;
const PADDED: u8 = 8;
const PRIORITY: u8 = 0x20;
const MAX_FRAME: usize = 16_384;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).unwrap()
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    /// Printable text of `min` to `min + spread - 1` octets.
    fn text(&mut self, min: usize, spread: usize) -> Vec<u8> {
        let len = min + self.below(spread);
        (0..len)
            .map(|_| 32 + u8::try_from(self.below(95)).unwrap())
            .collect()
    }
}

struct Frame {
    kind: u8,
    flags: u8,
    stream: u32,
    payload: Vec<u8>,
}

async fn read_frame(io: &mut (impl AsyncRead + Unpin)) -> std::io::Result<Frame> {
    let mut head = [0u8; 9];
    io.read_exact(&mut head).await?;
    let len = usize::from(head[0]) << 16 | usize::from(head[1]) << 8 | usize::from(head[2]);
    let mut payload = vec![0; len];
    io.read_exact(&mut payload).await?;
    let stream = u32::from_be_bytes([head[5] & 0x7f, head[6], head[7], head[8]]);
    Ok(Frame {
        kind: head[3],
        flags: head[4],
        stream,
        payload,
    })
}

async fn write_frame(
    io: &mut (impl AsyncWrite + Unpin),
    kind: u8,
    flags: u8,
    stream: u32,
    payload: &[u8],
) {
    let len = u32::try_from(payload.len()).unwrap().to_be_bytes();
    let mut out = vec![len[1], len[2], len[3], kind, flags];
    out.extend_from_slice(&stream.to_be_bytes());
    out.extend_from_slice(payload);
    io.write_all(&out).await.unwrap();
}

/// The header block of the HEADERS frame `first` plus its CONTINUATION frames.
async fn read_block(io: &mut (impl AsyncRead + Unpin), first: Frame) -> Vec<u8> {
    let mut payload = first.payload;
    if first.flags & PADDED != 0 {
        let pad = usize::from(payload[0]);
        payload = payload[1..payload.len() - pad].to_vec();
    }
    if first.flags & PRIORITY != 0 {
        payload.drain(..5);
    }
    let mut done = first.flags & END_HEADERS != 0;
    while !done {
        let next = read_frame(io).await.unwrap();
        assert_eq!(next.kind, CONTINUATION);
        payload.extend_from_slice(&next.payload);
        done = next.flags & END_HEADERS != 0;
    }
    payload
}

async fn with_timeout<T>(work: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_mins(2), work)
        .await
        .expect("timed out")
}

async fn accept_preface(io: &mut (impl AsyncRead + AsyncWrite + Unpin)) {
    io.read_exact(&mut [0; 24]).await.unwrap();
    write_frame(io, SETTINGS, 0, 0, &[]).await;
}

fn sorted(mut fields: Fields) -> Fields {
    fields.sort();
    fields
}

const HEADER_NAMES: [&str; 14] = [
    "accept",
    "accept-encoding",
    "accept-language",
    "cache-control",
    "cookie",
    "referer",
    "user-agent",
    "x-request-id",
    "x-forwarded-for",
    "x-api-key",
    "if-none-match",
    "x-a",
    "x-b",
    "x-very-long-custom-header-name-that-is-not-in-any-table",
];

struct Sent {
    request: Request<()>,
    expected: Fields,
}

fn random_request(rng: &mut Rng, repeat_pool: &[Vec<u8>]) -> Sent {
    let method = *rng.pick(&["GET", "GET", "GET", "POST", "PUT", "DELETE"]);
    let authority = *rng.pick(&[
        "example.com",
        "www.example.com:8443",
        "a.test",
        "[2001:db8::1]:8080",
    ]);
    let mut path = format!(
        "/{}",
        String::from_utf8(
            rng.text(0, 30)
                .into_iter()
                .map(|b| if b"?# %\"<>\\^`{|}".contains(&b) {
                    b'x'
                } else {
                    b
                })
                .collect()
        )
        .unwrap()
    );
    if rng.below(2) == 0 {
        path = format!("{path}?q={}&n={}", rng.below(1000), rng.below(7));
    }
    let mut builder = Request::builder()
        .method(method)
        .uri(format!("https://{authority}{path}"));
    let mut expected: Fields = vec![
        (b":method".to_vec(), method.into()),
        (b":scheme".to_vec(), b"https".to_vec()),
        (b":authority".to_vec(), authority.into()),
        (b":path".to_vec(), path.into()),
    ];
    for _ in 0..rng.below(10) {
        let name = *rng.pick(&HEADER_NAMES);
        let value = match rng.below(10) {
            0 if name == "cookie" => rng.text(15_000, 8000),
            1 | 2 => rng.text(0, 400),
            3 => rng.pick(repeat_pool).clone(),
            _ => rng.text(0, 24),
        };
        let mut header = HeaderValue::from_bytes(&value).unwrap();
        header.set_sensitive(rng.below(6) == 0);
        builder = builder.header(name, header);
        expected.push((name.into(), value));
    }
    Sent {
        request: builder.body(()).unwrap(),
        expected,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn blocks_from_the_h2_client_encoder_decode_to_the_fields_it_was_given() {
    let (client_io, mut server_io) = duplex(1 << 22);
    let (send, connection) = h2::client::handshake(client_io).await.unwrap();
    let driver = tokio::spawn(connection);
    let server = tokio::spawn(async move {
        accept_preface(&mut server_io).await;
        let mut dec = Decoder::new(4096);
        let mut seen = Vec::new();
        while let Ok(frame) = read_frame(&mut server_io).await {
            match frame.kind {
                SETTINGS if frame.flags & ACK == 0 => {
                    write_frame(&mut server_io, SETTINGS, ACK, 0, &[]).await;
                }
                HEADERS => {
                    let stream = frame.stream;
                    let block = read_block(&mut server_io, frame).await;
                    let mut fields = Fields::new();
                    let outcome = dec
                        .decode(&block, usize::MAX, |n, v| {
                            fields.push((n.to_vec(), v.to_vec()));
                        })
                        .expect("h2 encoder output must decode");
                    assert!(!outcome.list_too_large);
                    seen.push(fields);
                    write_frame(
                        &mut server_io,
                        HEADERS,
                        END_HEADERS | END_STREAM,
                        stream,
                        &[0x89],
                    )
                    .await;
                }
                _ => {}
            }
        }
        seen
    });
    let mut rng = Rng(0x5eed_0001_dead_beef);
    let repeat_pool: Vec<Vec<u8>> = (0..6).map(|n| rng.text(10 + n * 40, 1)).collect();
    let mut expected = Vec::new();
    let mut send: SendRequest<Bytes> = send;
    for _ in 0..500 {
        let generated = random_request(&mut rng, &repeat_pool);
        send = with_timeout(send.ready()).await.unwrap();
        let (response, _) = send.send_request(generated.request, true).unwrap();
        assert_eq!(with_timeout(response).await.unwrap().status(), 204);
        expected.push(sorted(generated.expected));
    }
    drop(send);
    with_timeout(driver).await.unwrap().unwrap();
    let seen = with_timeout(server).await.unwrap();
    assert_eq!(seen.len(), expected.len());
    for (i, (got, want)) in seen.into_iter().zip(expected).enumerate() {
        assert_eq!(
            &got[..4]
                .iter()
                .map(|(n, _)| n.as_slice())
                .collect::<Vec<_>>(),
            &[b":method".as_slice(), b":scheme", b":authority", b":path"]
        );
        assert_eq!(sorted(got), want, "request {i}");
    }
}

fn random_response(rng: &mut Rng, pool: &[(Vec<u8>, Vec<u8>)]) -> (Fields, Vec<Indexing>) {
    const NAMES: [&str; 12] = [
        "content-type",
        "cache-control",
        "server",
        "date",
        "etag",
        "last-modified",
        "vary",
        "set-cookie",
        "x-request-id",
        "location",
        "x-a",
        "x-very-long-custom-header-name-that-is-not-in-any-table",
    ];
    let mut fields: Fields = vec![(
        b":status".to_vec(),
        rng.pick(&["200", "204", "301", "404", "500", "418"])
            .as_bytes()
            .to_vec(),
    )];
    for _ in 0..rng.below(9) {
        fields.push(match rng.below(4) {
            0 if !pool.is_empty() => rng.pick(pool).clone(),
            1 => (rng.pick(&NAMES).as_bytes().to_vec(), rng.text(0, 300)),
            _ => (rng.pick(&NAMES).as_bytes().to_vec(), rng.text(0, 20)),
        });
    }
    let modes = fields
        .iter()
        .map(|_| {
            *rng.pick(&[
                Indexing::Incremental,
                Indexing::Incremental,
                Indexing::None,
                Indexing::Never,
            ])
        })
        .collect();
    (fields, modes)
}

async fn run_encoder_against_h2_client(client_table: Option<u32>, responses: usize, entropy: u64) {
    let (client_io, mut server_io) = duplex(1 << 22);
    let mut builder = Builder::new();
    if let Some(size) = client_table {
        builder.header_table_size(size);
    }
    let (send, connection) = builder.handshake::<_, Bytes>(client_io).await.unwrap();
    let driver = tokio::spawn(connection);
    let mut rng = Rng(entropy);
    let mut pool: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut plan = Vec::new();
    for _ in 0..responses {
        let (fields, modes) = random_response(&mut rng, &pool);
        pool.extend(fields.iter().skip(1).cloned());
        pool.truncate(30);
        plan.push((fields, modes));
    }
    let server_plan = plan.clone();
    let table = client_table.map_or(4096, |size| size as usize);
    let server = tokio::spawn(async move {
        accept_preface(&mut server_io).await;
        let mut encoder = Encoder::new(table);
        let mut next = server_plan.into_iter();
        while let Ok(frame) = read_frame(&mut server_io).await {
            match frame.kind {
                SETTINGS if frame.flags & ACK == 0 => {
                    write_frame(&mut server_io, SETTINGS, ACK, 0, &[]).await;
                }
                HEADERS => {
                    let stream = frame.stream;
                    read_block(&mut server_io, frame).await;
                    let (fields, modes) = next.next().expect("one response per request");
                    let mut block = Vec::new();
                    for ((name, value), mode) in fields.iter().zip(modes) {
                        encoder.encode_field(&mut block, name, value, mode);
                    }
                    let mut parts = block
                        .chunks(MAX_FRAME)
                        .map(<[u8]>::to_vec)
                        .collect::<Vec<_>>();
                    parts.resize(parts.len().max(1), Vec::new());
                    let last = parts.len() - 1;
                    for (i, part) in parts.iter().enumerate() {
                        let (kind, mut flags) = if i == 0 {
                            (HEADERS, END_STREAM)
                        } else {
                            (CONTINUATION, 0)
                        };
                        if i == last {
                            flags |= END_HEADERS;
                        }
                        write_frame(&mut server_io, kind, flags, stream, part).await;
                    }
                }
                _ => {}
            }
        }
    });
    let mut send: SendRequest<Bytes> = send;
    for (i, (fields, _)) in plan.iter().enumerate() {
        send = with_timeout(send.ready()).await.unwrap();
        let request = Request::builder()
            .uri("https://example.com/")
            .body(())
            .unwrap();
        let (response, _) = send.send_request(request, true).unwrap();
        let response = with_timeout(response)
            .await
            .unwrap_or_else(|e| panic!("response {i}: {e}"));
        let mut got: Fields = response
            .headers()
            .iter()
            .map(|(n, v)| (n.as_str().into(), v.as_bytes().to_vec()))
            .collect();
        got.push((b":status".to_vec(), response.status().as_str().into()));
        assert_eq!(sorted(got), sorted(fields.clone()), "response {i}");
    }
    drop(send);
    with_timeout(driver).await.unwrap().unwrap();
    with_timeout(server).await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn blocks_from_this_encoder_decode_in_the_h2_client_with_the_default_table() {
    run_encoder_against_h2_client(None, 300, 0xabad_1dea_0000_0001).await;
}

#[tokio::test(flavor = "current_thread")]
async fn blocks_from_this_encoder_decode_in_the_h2_client_with_small_tables() {
    run_encoder_against_h2_client(Some(128), 200, 0xabad_1dea_0000_0002).await;
    run_encoder_against_h2_client(Some(0), 100, 0xabad_1dea_0000_0003).await;
}

#[tokio::test(flavor = "current_thread")]
async fn blocks_from_this_encoder_decode_in_the_h2_client_with_a_table_larger_than_it_keeps() {
    run_encoder_against_h2_client(Some(65_536), 200, 0xabad_1dea_0000_0004).await;
}
