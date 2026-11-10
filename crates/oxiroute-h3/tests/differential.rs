//! Randomized header sets (seeded xorshift) through the thin layer and the `h3` crate, in both directions, and a
//! reference QPACK encoder written here for the raw client.
mod common;

use std::collections::BTreeMap;

use bytes::{Buf, Bytes};
use common::raw::{Raw, frame_bytes, read_frames, status_of};
use common::{fixture, within};
use http::{HeaderName, HeaderValue, Request, Response, StatusCode};
use oxiroute_h2::hpack::shared::{huffman_encode, integer_encode};
use oxiroute_h3::frame;
use oxiroute_h3::qpack::static_table::ENTRIES;
use oxiroute_h3::server::{Config, RequestStream};

const SEEDS: u64 = 150;
const FORBIDDEN: [&str; 8] = [
    "host",
    "connection",
    "te",
    "transfer-encoding",
    "keep-alive",
    "upgrade",
    "proxy-connection",
    "content-length",
];

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

type Fields = Vec<(String, String)>;

fn random_name(rng: &mut Rng) -> String {
    loop {
        let name = if rng.below(2) == 0 {
            let (name, _) = ENTRIES[rng.below(ENTRIES.len() as u64) as usize];
            name.to_string()
        } else {
            let len = 1 + rng.below(40);
            (0..len)
                .map(|_| b"abcdefghijklmnopqrstuvwxyz0123456789-"[rng.below(37) as usize] as char)
                .collect()
        };
        if !name.starts_with(':') && !FORBIDDEN.contains(&name.as_str()) {
            return name;
        }
    }
}

fn random_value(rng: &mut Rng) -> String {
    let len = match rng.below(10) {
        0 => 130 + rng.below(300),
        1 => 0,
        _ => rng.below(30),
    };
    let text: String = (0..len)
        .map(|_| (0x20 + rng.below(0x5f) as u8) as char)
        .collect();
    text.trim().to_string()
}

fn random_fields(seed: u64) -> Fields {
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    (0..1 + rng.below(12))
        .map(|_| {
            if rng.below(3) == 0 {
                let (name, value) = ENTRIES[rng.below(ENTRIES.len() as u64) as usize];
                if !name.starts_with(':') && !FORBIDDEN.contains(&name) {
                    return (name.to_string(), value.to_string());
                }
            }
            (random_name(&mut rng), random_value(&mut rng))
        })
        .collect()
}

fn multimap(fields: &Fields) -> BTreeMap<String, Vec<String>> {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, value) in fields {
        map.entry(name.clone()).or_default().push(value.clone());
    }
    map
}

fn parse_echo(body: &[u8]) -> BTreeMap<String, Vec<String>> {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in String::from_utf8(body.to_vec())
        .unwrap()
        .split_terminator('\n')
    {
        let (name, value) = line.split_once('\t').unwrap();
        map.entry(name.to_string())
            .or_default()
            .push(value.to_string());
    }
    map
}

/// Echoes the request fields as the body and answers with the fields and status derived from the seed in the path.
async fn handler(mut stream: RequestStream) {
    let request = stream.read_head().await.unwrap();
    let seed: u64 = request.uri().path()[1..].parse().unwrap();
    let mut echo = String::new();
    for (name, value) in request.headers() {
        echo.push_str(&format!("{}\t{}\n", name, value.to_str().unwrap()));
    }
    let mut response = Response::builder().status(response_status(seed));
    for (name, value) in random_fields(seed ^ 0xabcd) {
        response = response.header(
            HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_str(&value).unwrap(),
        );
    }
    stream
        .send_full_response(response.body(()).unwrap(), Bytes::from(echo), false)
        .await
        .unwrap();
}

fn response_status(seed: u64) -> StatusCode {
    StatusCode::from_u16(200 + (Rng(seed | 1).next() % 400) as u16).unwrap()
}

fn config() -> Config {
    Config {
        max_field_section_size: 1 << 20,
        ..Config::default()
    }
}

#[tokio::test]
async fn h3_client_requests_and_thin_responses_round_trip() {
    let fx = fixture();
    fx.serve(config(), handler);
    let (mut client, _conn) = fx.h3_client().await;
    within(async {
        for seed in 1..=SEEDS {
            let sent = random_fields(seed);
            let mut builder = Request::get(format!("https://example.com/{seed}"));
            for (name, value) in &sent {
                builder = builder.header(name.as_str(), value.as_str());
            }
            let mut stream = client
                .send_request(builder.body(()).unwrap())
                .await
                .unwrap();
            stream.finish().await.unwrap();
            let response = stream.recv_response().await.unwrap();
            assert_eq!(response.status(), response_status(seed), "seed {seed}");
            let mut got = Vec::new();
            for name in response.headers().keys() {
                for value in response.headers().get_all(name) {
                    got.push((name.to_string(), value.to_str().unwrap().to_string()));
                }
            }
            assert_eq!(
                multimap(&got),
                multimap(&random_fields(seed ^ 0xabcd)),
                "response fields, seed {seed}"
            );
            let mut body = Vec::new();
            while let Some(mut chunk) = stream.recv_data().await.unwrap() {
                body.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()));
            }
            assert_eq!(
                parse_echo(&body),
                multimap(&sent),
                "request fields, seed {seed}"
            );
        }
    })
    .await;
}

fn put_string(out: &mut Vec<u8>, bytes: &[u8], huffman: bool) {
    if huffman {
        let mut coded = Vec::new();
        huffman_encode(bytes, &mut coded);
        integer_encode(out, 0x80, 7, coded.len());
        out.extend_from_slice(&coded);
    } else {
        integer_encode(out, 0x00, 7, bytes.len());
        out.extend_from_slice(bytes);
    }
}

/// The reference encoder: every field gets a random one of the legal representations without a dynamic table.
fn reference_block(seed: u64, fields: &Fields, rng: &mut Rng) -> Vec<u8> {
    let mut block = vec![0, 0, 0xd1, 0xd7, 0x50]; // :method GET, :scheme https, :authority literal
    put_string(&mut block, b"example.com", rng.below(2) == 0);
    block.push(0x51); // :path with a static name reference
    put_string(&mut block, format!("/{seed}").as_bytes(), rng.below(2) == 0);
    for (name, value) in fields {
        let exact = ENTRIES.iter().position(|&(n, v)| n == name && v == value);
        let name_index = ENTRIES.iter().position(|&(n, _)| n == name);
        match (exact, name_index, rng.below(3)) {
            (Some(index), _, 0..=1) => integer_encode(&mut block, 0xc0, 6, index),
            (_, Some(index), 0..=1) => {
                integer_encode(&mut block, 0x50, 4, index);
                put_string(&mut block, value.as_bytes(), rng.below(2) == 0);
            }
            _ => {
                let huffman = rng.below(2) == 0;
                if huffman {
                    let mut coded = Vec::new();
                    huffman_encode(name.as_bytes(), &mut coded);
                    integer_encode(&mut block, 0x28, 3, coded.len());
                    block.extend_from_slice(&coded);
                } else {
                    integer_encode(&mut block, 0x20, 3, name.len());
                    block.extend_from_slice(name.as_bytes());
                }
                put_string(&mut block, value.as_bytes(), rng.below(2) == 0);
            }
        }
    }
    block
}

#[tokio::test]
async fn reference_encoder_requests_decode_identically() {
    let fx = fixture();
    fx.serve(config(), handler);
    let conn = fx
        .client
        .connect(fx.addr, "localhost")
        .unwrap()
        .await
        .unwrap();
    let raw = Raw::new(conn).await;
    within(async {
        for seed in 1..=SEEDS * 2 {
            let sent = random_fields(seed);
            let mut rng = Rng(seed | 1);
            let block = reference_block(seed, &sent, &mut rng);
            let (mut send, mut recv) = raw.request(&frame_bytes(frame::HEADERS, &block)).await;
            send.finish().unwrap();
            let frames = read_frames(&mut recv).await.unwrap();
            assert_eq!(status_of(&frames[0].1), response_status(seed).as_u16());
            assert_eq!(frames[1].0, frame::DATA);
            assert_eq!(parse_echo(&frames[1].1), multimap(&sent), "seed {seed}");
        }
    })
    .await;
}
