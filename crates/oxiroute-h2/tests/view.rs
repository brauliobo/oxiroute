//! `accept_view` against `accept`: the same client bytes must produce the same request heads (the view's accessors,
//! `into_parts`, and the `Parts` `accept` returns) and the same reactions (resets, 431, GOAWAY) in every mode.
#![allow(
    clippy::cast_possible_truncation,
    clippy::doc_markdown,
    clippy::too_many_lines
)]
mod support;
use std::sync::{Arc, Mutex};

use http::request::Parts;
use http::{HeaderMap, StatusCode};
use oxiroute_h2::hpack::{Encoder, Indexing};
use oxiroute_h2::server::{Config, RequestView, Server};
use support::Reaction::{Reset, Status};
use support::*;
use tokio::io::{AsyncWriteExt, DuplexStream, duplex};

type Bytes2 = (Vec<u8>, Vec<u8>);

#[derive(Debug, PartialEq)]
struct Head {
    method: String,
    uri: String,
    scheme: Option<String>,
    authority: Option<String>,
    path_and_query: Option<String>,
    headers: Vec<(String, Vec<u8>)>,
    version: http::Version,
    extensions: usize,
}

fn head(p: &Parts) -> Head {
    Head {
        method: p.method.as_str().into(),
        uri: p.uri.to_string(),
        scheme: p.uri.scheme_str().map(Into::into),
        authority: p.uri.authority().map(|a| a.as_str().into()),
        path_and_query: p.uri.path_and_query().map(|x| x.as_str().into()),
        headers: p
            .headers
            .iter()
            .map(|(n, v)| (n.as_str().into(), v.as_bytes().to_vec()))
            .collect(),
        version: p.version,
        extensions: p.extensions.len(),
    }
}

#[derive(Debug)]
struct Snap {
    method: Vec<u8>,
    scheme: Option<Vec<u8>>,
    authority: Option<Vec<u8>>,
    path: Vec<u8>,
    fields: Vec<Bytes2>,
    content_length: Option<u64>,
    end_stream: bool,
}

fn snap(v: &RequestView) -> Snap {
    Snap {
        method: v.method().to_vec(),
        scheme: v.scheme().map(<[u8]>::to_vec),
        authority: v.authority().map(<[u8]>::to_vec),
        path: v.path().to_vec(),
        fields: v.fields().map(|(n, v)| (n.to_vec(), v.to_vec())).collect(),
        content_length: v.content_length(),
        end_stream: v.is_end_stream(),
    }
}

/// The view's content is the `Parts` content; the scheme is only part of a URI that has an authority.
fn assert_same(s: &Snap, h: &Head) {
    assert_eq!(s.method, h.method.as_bytes());
    assert_eq!(
        s.authority.as_deref(),
        h.authority.as_deref().map(str::as_bytes)
    );
    let scheme = s.scheme.as_deref().filter(|_| s.authority.is_some());
    assert_eq!(scheme, h.scheme.as_deref().map(str::as_bytes));
    assert_eq!(
        &s.path[..],
        h.path_and_query.as_deref().unwrap_or_default().as_bytes()
    );
    let mut map = HeaderMap::new();
    for (n, v) in &s.fields {
        map.append(
            http::HeaderName::from_lowercase(n).unwrap(),
            http::HeaderValue::from_bytes(v).unwrap(),
        );
    }
    let from_view: Vec<_> = map
        .iter()
        .map(|(n, v)| (n.as_str().to_string(), v.as_bytes().to_vec()))
        .collect();
    assert_eq!(from_view, h.headers);
    let declared = h.headers.iter().find(|(n, _)| n == "content-length");
    let declared = declared.map(|(_, v)| std::str::from_utf8(v).unwrap().parse::<u64>().unwrap());
    assert_eq!(s.content_length, declared);
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Via {
    Classic,
    View,
    /// `accept` and `accept_view` alternate request by request.
    Mixed,
}

struct Outcome {
    heads: Vec<Head>,
    snaps: Vec<Snap>,
    reactions: Vec<Reaction>,
}

async fn serve(
    io: DuplexStream,
    config: Config,
    via: Via,
    out: Arc<Mutex<(Vec<Head>, Vec<Snap>)>>,
) {
    let Ok(mut server) = Server::handshake(io, config).await else {
        return;
    };
    for n in 0.. {
        let classic = via == Via::Classic || (via == Via::Mixed && n % 2 == 1);
        let (h, s, mut stream) = if classic {
            let Some(Ok((parts, stream))) = server.accept().await else {
                return;
            };
            (head(&parts), None, stream)
        } else {
            let Some(Ok((view, stream))) = server.accept_view().await else {
                return;
            };
            let s = snap(&view);
            (head(&view.into_parts()), Some(s), stream)
        };
        let mut o = out.lock().unwrap();
        o.0.push(h);
        o.1.extend(s);
        drop(o);
        let _ = stream.send_response(StatusCode::OK, HeaderMap::new(), true);
    }
}

fn run(steps: &[Vec<u8>], config: Config, via: Via) -> Outcome {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .unwrap();
    rt.block_on(async {
        let (client, server) = duplex(1 << 22);
        let seen = Arc::new(Mutex::new((Vec::new(), Vec::new())));
        tokio::spawn(serve(server, config, via, seen.clone()));
        let mut wire = Wire::new(client);
        wire.start().await;
        wire.settle().await;
        for step in steps {
            let _ = wire.io.write_all(step).await;
            let _ = wire.io.flush().await;
            wire.settle().await;
        }
        let frames = wire.frames.clone();
        let reactions = wire.reactions(&frames);
        let (heads, snaps) = std::mem::take(&mut *seen.lock().unwrap());
        Outcome {
            heads,
            snaps,
            reactions,
        }
    })
}

/// Runs the steps in every mode and checks that they all agree; returns the classic outcome.
fn agree(name: &str, steps: &[Vec<u8>], config: Config) -> Outcome {
    let classic = run(steps, config, Via::Classic);
    let view = run(steps, config, Via::View);
    let mixed = run(steps, config, Via::Mixed);
    assert_eq!(classic.heads, view.heads, "{name}: into_parts is accept");
    assert_eq!(
        classic.heads, mixed.heads,
        "{name}: mixed accept and accept_view"
    );
    assert_eq!(classic.reactions, view.reactions, "{name}: reactions");
    assert_eq!(
        classic.reactions, mixed.reactions,
        "{name}: reactions (mixed)"
    );
    for (s, h) in view.snaps.iter().zip(&view.heads) {
        assert_same(s, h);
    }
    classic
}

fn block(fields: &[(&[u8], &[u8])]) -> Vec<u8> {
    let mut enc = Encoder::new(4096);
    let mut out = Vec::new();
    for (n, v) in fields {
        enc.encode_field(&mut out, n, v, Indexing::None);
    }
    out
}

fn request(id: u32, fields: &[(&[u8], &[u8])], end: bool) -> Vec<u8> {
    headers(
        id,
        &block(fields),
        END_HEADERS | if end { END_STREAM } else { 0 },
    )
}

const BASE: [(&[u8], &[u8]); 4] = [
    (b":method", b"GET"),
    (b":scheme", b"https"),
    (b":authority", b"example.test"),
    (b":path", b"/"),
];

fn with<'a>(extra: &[(&'a [u8], &'a [u8])]) -> Vec<(&'a [u8], &'a [u8])> {
    BASE.iter().chain(extra).copied().collect()
}

fn config() -> Config {
    Config::default()
}

#[test]
fn a_plain_get() {
    let steps = [request(1, &BASE, true)];
    let out = agree("get", &steps, config());
    assert_eq!(out.reactions, vec![Status(1, 200)]);
    let s = &run(&steps, config(), Via::View).snaps[0];
    assert_eq!(s.method, b"GET");
    assert_eq!(s.scheme.as_deref(), Some(&b"https"[..]));
    assert_eq!(s.authority.as_deref(), Some(&b"example.test"[..]));
    assert_eq!(s.path, b"/");
    assert!(s.fields.is_empty());
    assert_eq!(s.content_length, None);
    assert!(s.end_stream);
}

#[test]
fn fields_keep_wire_order_duplicates_and_values() {
    let extra: &[(&[u8], &[u8])] = &[
        (b"x-b", b"1"),
        (b"accept", b"*/*"),
        (b"x-b", b"2"),
        (b"cookie", b"a=b"),
        (b"cookie", b"c=d"),
        (b"x-empty", b""),
        (b"content-length", b"3"),
        (b"content-length", b"3"),
    ];
    let steps = [request(1, &with(extra), false)];
    let view = run(&steps, config(), Via::View);
    let want: Vec<Bytes2> = extra
        .iter()
        .map(|(n, v)| (n.to_vec(), v.to_vec()))
        .collect();
    assert_eq!(view.snaps[0].fields, want);
    assert_eq!(view.snaps[0].content_length, Some(3));
    assert!(!view.snaps[0].end_stream);
    agree("fields", &steps, config());
}

#[test]
fn connect_has_no_scheme_or_path() {
    let fields: [(&[u8], &[u8]); 2] = [
        (b":method", b"CONNECT"),
        (b":authority", b"example.test:443"),
    ];
    let steps = [request(1, &fields, false)];
    let view = run(&steps, config(), Via::View);
    let s = &view.snaps[0];
    assert_eq!((s.scheme.as_deref(), s.path.as_slice()), (None, &b""[..]));
    assert_eq!(s.authority.as_deref(), Some(&b"example.test:443"[..]));
    agree("connect", &steps, config());
}

#[test]
fn a_fragment_is_cut_like_http_does() {
    for path in [&b"/a?b#c"[..], b"/a#\xff", b"#only", b"/p?q=\xc3\xa9"] {
        let steps = [request(
            1,
            &[BASE[0], BASE[1], BASE[2], (b":path", path)],
            true,
        )];
        agree("fragment", &steps, config());
    }
    let steps = [request(
        1,
        &[BASE[0], BASE[1], BASE[2], (b":path", b"/a?b#c")],
        true,
    )];
    assert_eq!(run(&steps, config(), Via::View).snaps[0].path, b"/a?b");
}

type Case = (&'static str, Vec<(&'static [u8], &'static [u8])>);

fn malformed_cases() -> Vec<Case> {
    let m = |f: &[(&'static [u8], &'static [u8])]| f.to_vec();
    vec![
        ("no method", m(&BASE[1..])),
        ("no scheme", m(&[BASE[0], BASE[2], BASE[3]])),
        ("no path", m(&BASE[..3])),
        (
            "empty path",
            m(&[BASE[0], BASE[1], BASE[2], (b":path", b"")]),
        ),
        (
            "bad path",
            m(&[BASE[0], BASE[1], BASE[2], (b":path", b"/a b")]),
        ),
        (
            "path without slash",
            m(&[BASE[0], BASE[1], BASE[2], (b":path", b"a")]),
        ),
        (
            "asterisk path",
            m(&[BASE[0], BASE[1], BASE[2], (b":path", b"*")]),
        ),
        (
            "bad method",
            m(&[(b":method", b"G ET"), BASE[1], BASE[2], BASE[3]]),
        ),
        (
            "empty method",
            m(&[(b":method", b""), BASE[1], BASE[2], BASE[3]]),
        ),
        (
            "bad scheme",
            m(&[BASE[0], (b":scheme", b"ht tp"), BASE[2], BASE[3]]),
        ),
        (
            "other scheme",
            m(&[BASE[0], (b":scheme", b"x-y.z"), BASE[2], BASE[3]]),
        ),
        (
            "empty authority",
            m(&[BASE[0], BASE[1], (b":authority", b""), BASE[3]]),
        ),
        (
            "bad authority",
            m(&[BASE[0], BASE[1], (b":authority", b"a b"), BASE[3]]),
        ),
        ("duplicate method", with_pseudo(b":method", b"GET")),
        ("duplicate path", with_pseudo(b":path", b"/")),
        ("unknown pseudo", with_pseudo(b":status", b"200")),
        (
            "pseudo after regular",
            m(&[BASE[0], BASE[1], BASE[2], (b"accept", b"*"), BASE[3]]),
        ),
        ("uppercase name", with(&[(b"Accept", b"*/*")])),
        ("name with colon", with(&[(b"a:b", b"1")])),
        ("empty name", with(&[(b"", b"1")])),
        ("control in value", with(&[(b"x-a", b"a\x01b")])),
        ("newline in value", with(&[(b"x-a", b"a\nb")])),
        ("connection", with(&[(b"connection", b"close")])),
        (
            "transfer-encoding",
            with(&[(b"transfer-encoding", b"chunked")]),
        ),
        ("upgrade", with(&[(b"upgrade", b"h2c")])),
        ("keep-alive", with(&[(b"keep-alive", b"5")])),
        ("proxy-connection", with(&[(b"proxy-connection", b"x")])),
        ("te gzip", with(&[(b"te", b"gzip")])),
        ("te trailers", with(&[(b"te", b"trailers")])),
        ("content-length sign", with(&[(b"content-length", b"+1")])),
        ("content-length empty", with(&[(b"content-length", b"")])),
        (
            "content-length long",
            with(&[(b"content-length", b"12345678901234567890")]),
        ),
        (
            "content-length mismatch",
            with(&[(b"content-length", b"1"), (b"content-length", b"2")]),
        ),
        (
            "content-length with end stream",
            with(&[(b"content-length", b"1")]),
        ),
        (
            "connect with path",
            m(&[(b":method", b"CONNECT"), BASE[2], BASE[3]]),
        ),
        (
            "connect with scheme",
            m(&[(b":method", b"CONNECT"), BASE[1], BASE[2]]),
        ),
        ("connect without authority", m(&[(b":method", b"CONNECT")])),
        (
            "lowercase connect",
            m(&[(b":method", b"connect"), BASE[1], BASE[2], BASE[3]]),
        ),
        (
            "long method",
            m(&[
                (b":method", b"ABCDEFGHIJKLMNOPQRSTUVWXYZ"),
                BASE[1],
                BASE[2],
                BASE[3],
            ]),
        ),
    ]
}

fn with_pseudo(name: &'static [u8], value: &'static [u8]) -> Vec<(&'static [u8], &'static [u8])> {
    let mut f = BASE.to_vec();
    f.push((name, value));
    let at = f.len() - 1;
    f.swap(at, 1); // still before every regular field
    f
}

#[test]
fn malformed_requests_are_refused_alike() {
    for (name, fields) in malformed_cases() {
        for end in [true, false] {
            let out = agree(
                name,
                &[request(1, &fields, end), request(3, &BASE, true)],
                config(),
            );
            assert!(
                out.reactions.contains(&Status(3, 200)),
                "{name}: the connection carries on {:?}",
                out.reactions
            );
        }
    }
}

#[test]
fn the_refusals_are_stream_resets() {
    let out = agree(
        "reset",
        &[request(1, &with(&[(b"te", b"gzip")]), true)],
        config(),
    );
    assert_eq!(out.reactions, vec![Reset(1, 1)]);
    assert!(out.heads.is_empty());
}

#[test]
fn a_header_list_over_the_limit_is_answered_431_alike() {
    let mut c = Config::default();
    c.max_header_list_size(512);
    let big = vec![b'x'; 600];
    let steps = [
        request(1, &with(&[(b"x-big", &big)]), true),
        request(3, &with(&[(b"x-big", &big)]), false),
        request(5, &BASE, true),
    ];
    let out = agree("431", &steps, c);
    assert!(out.reactions.contains(&Status(1, 431)));
    assert!(out.reactions.contains(&Status(5, 200)));
}

#[test]
fn a_block_split_over_continuation_frames_is_the_same_request() {
    let b = block(&with(&[(b"x-a", b"1"), (b"x-b", b"2")]));
    let (a, rest) = b.split_at(b.len() / 2);
    let steps = [[
        headers(1, a, END_STREAM),
        continuation(1, rest, END_HEADERS),
    ]
    .concat()];
    let out = agree("continuation", &steps, config());
    assert_eq!(out.reactions, vec![Status(1, 200)]);
}

fn many_fields(n: usize) -> Vec<Vec<u8>> {
    let names: Vec<Vec<u8>> = (0..n).map(|i| format!("n{i}").into_bytes()).collect();
    let mut fields: Vec<(&[u8], &[u8])> = BASE.to_vec();
    fields.extend(names.iter().map(|k| (k.as_slice(), &b"v"[..])));
    let b = block(&fields);
    let mut chunks = b.chunks(16_000);
    let mut out = vec![headers(1, chunks.next().unwrap(), END_STREAM)];
    let rest: Vec<_> = chunks.collect();
    for (i, c) in rest.iter().enumerate() {
        out.push(continuation(
            1,
            c,
            if i + 1 == rest.len() { END_HEADERS } else { 0 },
        ));
    }
    out
}

#[test]
fn the_field_count_limit_is_the_same_in_every_mode() {
    let mut c = Config::default();
    c.max_header_list_size(4 << 20)
        .max_header_block_bytes(4 << 20)
        .max_continuation_frames(100);
    let ok = agree("12288 fields", &[many_fields(12_288).concat()], c);
    assert_eq!(ok.reactions, vec![Status(1, 200)]);
    let view = run(&[many_fields(12_288).concat()], c, Via::View);
    assert_eq!(view.snaps[0].fields.len(), 12_288);
    let over = agree("12289 fields", &[many_fields(12_289).concat()], c);
    assert_eq!(over.reactions, vec![Reset(1, 1)]);
}

#[test]
fn trailers_still_work_after_a_view() {
    let steps = [
        request(1, &with(&[(b"te", b"trailers")]), false),
        data(1, b"abc", 0),
        request(1, &[(b"x-trailer", b"1")], true),
    ];
    agree("trailers", &steps, config());
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a>(&mut self, items: &[&'a [u8]]) -> &'a [u8] {
        items[self.below(items.len())]
    }

    fn text(&mut self, alphabet: &[u8], max: usize) -> Vec<u8> {
        (0..self.below(max))
            .map(|_| alphabet[self.below(alphabet.len())])
            .collect()
    }
}

const METHODS: [&[u8]; 9] = [
    b"GET", b"GET", b"GET", b"POST", b"CONNECT", b"get", b"BREW", b"", b"G E",
];
const SCHEMES: [&[u8]; 6] = [b"https", b"https", b"http", b"", b"ht tp", b"x-y.z"];
const AUTHORITIES: [&[u8]; 7] = [
    b"example.test",
    b"example.test",
    b"example.test:8443",
    b"",
    b"a b",
    b"[::1]:80",
    b"u@h",
];
const NAMES: [&[u8]; 14] = [
    b"accept",
    b"user-agent",
    b"x-a",
    b"x-b",
    b"cookie",
    b"X-Upper",
    b"te",
    b"connection",
    b"content-length",
    b"a:b",
    b"",
    b"x-trace",
    b"accept",
    b"user-agent",
];
const PATH_ALPHABET: &[u8] = b"/?#abc%20=&~-_ \"\x00\x80\xc3\xa9\xff";
const VALUE_ALPHABET: &[u8] = b"abc 0123\t;=/*\x01\x7f\x80\n";

/// A random request: mostly valid (with repeated fields, as real clients send), sometimes broken in one of the
/// ways the cases above name.
fn random_request(rng: &mut Rng, id: u32, last: &mut Vec<(Vec<u8>, Vec<u8>)>) -> Vec<u8> {
    if rng.below(3) == 0 && !last.is_empty() {
        let f: Vec<(&[u8], &[u8])> = last.iter().map(|(n, v)| (&n[..], &v[..])).collect();
        return request(id, &f, rng.below(2) == 0);
    }
    let clean = rng.below(4) != 0;
    let mut path = b"/x".to_vec();
    if !clean {
        path = rng.text(PATH_ALPHABET, 12);
        if rng.below(4) != 0 {
            path.insert(0, b'/');
        }
    }
    let fixed = |rng: &mut Rng, ok: &[&'static [u8]], any: &[&'static [u8]]| {
        (if clean { rng.pick(ok) } else { rng.pick(any) }).to_vec()
    };
    let mut f: Vec<(Vec<u8>, Vec<u8>)> = vec![
        (b":method".to_vec(), fixed(rng, &METHODS[..4], &METHODS)),
        (b":scheme".to_vec(), fixed(rng, &SCHEMES[..3], &SCHEMES)),
        (
            b":authority".to_vec(),
            fixed(rng, &AUTHORITIES[..3], &AUTHORITIES),
        ),
        (b":path".to_vec(), path),
    ];
    for _ in 0..rng.below(6) {
        let (name, value) = if clean {
            (
                rng.pick(&NAMES[..5]).to_vec(),
                rng.text(b"abc 0123;=/*", 12),
            )
        } else {
            (rng.pick(&NAMES).to_vec(), rng.text(VALUE_ALPHABET, 12))
        };
        f.push((name, value));
    }
    if !clean && rng.below(8) == 0 {
        let at = rng.below(f.len());
        f.remove(at);
    }
    last.clone_from(&f);
    let f: Vec<(&[u8], &[u8])> = f.iter().map(|(n, v)| (&n[..], &v[..])).collect();
    request(id, &f, rng.below(2) == 0)
}

#[test]
fn random_requests_are_the_same_in_every_mode() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let (mut accepted, mut refused) = (0, 0);
    for round in 0..300 {
        let mut last = Vec::new();
        let steps: Vec<Vec<u8>> = (0..8)
            .map(|i| random_request(&mut rng, 2 * i + 1, &mut last))
            .collect();
        let out = agree(&format!("random {round}"), &[steps.concat()], config());
        accepted += out.heads.len();
        refused += out
            .reactions
            .iter()
            .filter(|r| matches!(r, Reset(..)))
            .count();
    }
    assert!(
        accepted > 300 && refused > 300,
        "accepted {accepted}, refused {refused}"
    );
}
