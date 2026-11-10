//! HPACK (RFC 7541) decoder and encoder with an arena-backed dynamic table (no allocation per entry).
use super::huffman;
use std::collections::{HashMap, VecDeque};
use std::hash::BuildHasherDefault;
use std::sync::OnceLock;

pub const STATIC: [(&[u8], &[u8]); 61] = [
    (b":authority", b""),
    (b":method", b"GET"),
    (b":method", b"POST"),
    (b":path", b"/"),
    (b":path", b"/index.html"),
    (b":scheme", b"http"),
    (b":scheme", b"https"),
    (b":status", b"200"),
    (b":status", b"204"),
    (b":status", b"206"),
    (b":status", b"304"),
    (b":status", b"400"),
    (b":status", b"404"),
    (b":status", b"500"),
    (b"accept-charset", b""),
    (b"accept-encoding", b"gzip, deflate"),
    (b"accept-language", b""),
    (b"accept-ranges", b""),
    (b"accept", b""),
    (b"access-control-allow-origin", b""),
    (b"age", b""),
    (b"allow", b""),
    (b"authorization", b""),
    (b"cache-control", b""),
    (b"content-disposition", b""),
    (b"content-encoding", b""),
    (b"content-language", b""),
    (b"content-length", b""),
    (b"content-location", b""),
    (b"content-range", b""),
    (b"content-type", b""),
    (b"cookie", b""),
    (b"date", b""),
    (b"etag", b""),
    (b"expect", b""),
    (b"expires", b""),
    (b"from", b""),
    (b"host", b""),
    (b"if-match", b""),
    (b"if-modified-since", b""),
    (b"if-none-match", b""),
    (b"if-range", b""),
    (b"if-unmodified-since", b""),
    (b"last-modified", b""),
    (b"link", b""),
    (b"location", b""),
    (b"max-forwards", b""),
    (b"proxy-authenticate", b""),
    (b"proxy-authorization", b""),
    (b"range", b""),
    (b"referer", b""),
    (b"refresh", b""),
    (b"retry-after", b""),
    (b"server", b""),
    (b"set-cookie", b""),
    (b"strict-transport-security", b""),
    (b"transfer-encoding", b""),
    (b"user-agent", b""),
    (b"vary", b""),
    (b"via", b""),
    (b"www-authenticate", b""),
];

#[derive(Debug)]
pub struct Error;
type R<T> = Result<T, Error>;

#[derive(Clone, Copy)]
struct Entry {
    off: usize,
    nlen: u32,
    vlen: u32,
    hash: u64,
    id: u64,
}

impl Entry {
    fn size(&self) -> usize {
        (self.nlen + self.vlen) as usize + 32
    }
}

/// Dynamic table: entries index into one byte arena with absolute offsets; evicted bytes are reclaimed in bulk.
pub struct Table {
    entries: VecDeque<Entry>,
    arena: Vec<u8>,
    origin: usize,
    size: usize,
    max: usize,
    cap: usize,
    inserted: u64,
}

impl Table {
    pub fn new(cap: usize) -> Self {
        Table {
            entries: VecDeque::new(),
            arena: Vec::new(),
            origin: 0,
            size: 0,
            max: cap,
            cap,
            inserted: 0,
        }
    }

    pub fn set_max(&mut self, max: usize) -> R<()> {
        if max > self.cap {
            return Err(Error);
        }
        self.max = max;
        self.evict(0);
        Ok(())
    }

    fn evict(&mut self, extra: usize) -> Vec<Entry> {
        let mut gone = vec![];
        while self.size + extra > self.max {
            let Some(e) = self.entries.pop_front() else {
                break;
            };
            self.size -= e.size();
            gone.push(e);
        }
        match self.entries.front() {
            None => {
                self.origin += self.arena.len();
                self.arena.clear();
            }
            Some(e) if e.off - self.origin > 4096 => {
                self.arena.drain(..e.off - self.origin);
                self.origin = e.off;
            }
            _ => {}
        }
        gone
    }

    /// Returns the evicted entries' (hash, id) so the encoder can drop its index.
    pub fn insert(&mut self, name: &[u8], value: &[u8], hash: u64) -> Vec<Entry> {
        let need = name.len() + value.len() + 32;
        let gone = self.evict(need);
        if need <= self.max {
            let off = self.origin + self.arena.len();
            self.arena.extend_from_slice(name);
            self.arena.extend_from_slice(value);
            self.entries.push_back(Entry {
                off,
                nlen: name.len() as u32,
                vlen: value.len() as u32,
                hash,
                id: self.inserted,
            });
            self.size += need;
            self.inserted += 1;
        }
        gone
    }

    fn slices(&self, e: &Entry) -> (&[u8], &[u8]) {
        let s = e.off - self.origin;
        (
            &self.arena[s..s + e.nlen as usize],
            &self.arena[s + e.nlen as usize..s + (e.nlen + e.vlen) as usize],
        )
    }

    /// 1 is the newest entry (HPACK index 62).
    fn get(&self, dyn_index: usize) -> R<(&[u8], &[u8])> {
        let n = self.entries.len();
        if dyn_index == 0 || dyn_index > n {
            return Err(Error);
        }
        Ok(self.slices(&self.entries[n - dyn_index]))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Default)]
pub struct Fields {
    pub buf: Vec<u8>,
    items: Vec<(u32, u32, u32)>,
}

impl Fields {
    pub fn clear(&mut self) {
        self.buf.clear();
        self.items.clear();
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn iter(&self) -> impl Iterator<Item = (&[u8], &[u8])> {
        self.items.iter().map(|&(a, b, c)| {
            (
                &self.buf[a as usize..b as usize],
                &self.buf[b as usize..c as usize],
            )
        })
    }
}

pub struct Decoder {
    pub table: Table,
}

fn int(src: &mut &[u8], prefix: u32) -> R<usize> {
    let (&b, rest) = src.split_first().ok_or(Error)?;
    *src = rest;
    let mask = (1usize << prefix) - 1;
    let mut v = b as usize & mask;
    if v < mask {
        return Ok(v);
    }
    let mut shift = 0;
    loop {
        let (&b, rest) = src.split_first().ok_or(Error)?;
        *src = rest;
        v += ((b & 0x7f) as usize) << shift;
        shift += 7;
        if b & 0x80 == 0 {
            return Ok(v);
        }
        if shift > 28 {
            return Err(Error);
        }
    }
}

fn string(src: &mut &[u8], out: &mut Vec<u8>) -> R<()> {
    let huff = src.first().ok_or(Error)? & 0x80 != 0;
    let len = int(src, 7)?;
    if src.len() < len {
        return Err(Error);
    }
    let (s, rest) = src.split_at(len);
    *src = rest;
    if huff {
        huffman::decode(s, out).map_err(|_| Error)
    } else {
        Ok(out.extend_from_slice(s))
    }
}

impl Decoder {
    pub fn new(table_cap: usize) -> Self {
        Decoder {
            table: Table::new(table_cap),
        }
    }

    fn name_into(&self, idx: usize, out: &mut Vec<u8>) -> R<()> {
        let n = if idx <= 61 {
            STATIC[idx - 1].0
        } else {
            self.table.get(idx - 61)?.0
        };
        Ok(out.extend_from_slice(n))
    }

    pub fn decode(&mut self, mut src: &[u8], out: &mut Fields) -> R<()> {
        while let Some(&b) = src.first() {
            let (a, name_end);
            if b & 0x80 != 0 {
                let idx = int(&mut src, 7)?;
                if idx == 0 {
                    return Err(Error);
                }
                let (n, v) = if idx <= 61 {
                    STATIC[idx - 1]
                } else {
                    self.table.get(idx - 61)?
                };
                a = out.buf.len();
                out.buf.extend_from_slice(n);
                name_end = out.buf.len();
                out.buf.extend_from_slice(v);
            } else if b & 0xe0 == 0x20 {
                let max = int(&mut src, 5)?;
                self.table.set_max(max)?;
                continue;
            } else {
                let (prefix, index) = if b & 0x40 != 0 { (6, true) } else { (4, false) };
                let idx = int(&mut src, prefix)?;
                a = out.buf.len();
                if idx == 0 {
                    string(&mut src, &mut out.buf)?;
                } else {
                    self.name_into(idx, &mut out.buf)?;
                }
                name_end = out.buf.len();
                string(&mut src, &mut out.buf)?;
                if index {
                    let (n, v) = (&out.buf[a..name_end], &out.buf[name_end..]);
                    self.table.insert(n, v, 0);
                }
            }
            out.items
                .push((a as u32, name_end as u32, out.buf.len() as u32));
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct Fx(u64);
impl std::hash::Hasher for Fx {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        let mut h = self.0;
        let mut c = bytes.chunks_exact(8);
        for w in &mut c {
            h = (h.rotate_left(5) ^ u64::from_le_bytes(w.try_into().unwrap()))
                .wrapping_mul(0x517c_c1b7_2722_0a95);
        }
        for &b in c.remainder() {
            h = (h.rotate_left(5) ^ b as u64).wrapping_mul(0x517c_c1b7_2722_0a95);
        }
        self.0 = h;
    }
    fn write_u64(&mut self, v: u64) {
        self.0 = v;
    }
}
type Bh = BuildHasherDefault<Fx>;

pub fn hash2(name: &[u8], value: &[u8]) -> u64 {
    use std::hash::Hasher;
    let mut h = Fx(0);
    h.write(name);
    h.write(&[0xff]);
    h.write(value);
    h.0
}

fn statics() -> &'static (HashMap<&'static [u8], u8, Bh>, HashMap<u64, u8, Bh>) {
    static S: OnceLock<(HashMap<&'static [u8], u8, Bh>, HashMap<u64, u8, Bh>)> = OnceLock::new();
    S.get_or_init(|| {
        let (mut names, mut full) = (HashMap::default(), HashMap::default());
        for (i, (n, v)) in STATIC.iter().enumerate() {
            names.entry(*n).or_insert(i as u8 + 1);
            if !v.is_empty() {
                full.insert(hash2(n, v), i as u8 + 1);
            }
        }
        (names, full)
    })
}

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Index,
    NoIndex,
    Never,
}

fn put_int(out: &mut Vec<u8>, first: u8, prefix: u32, mut v: usize) {
    let mask = (1usize << prefix) - 1;
    if v < mask {
        out.push(first | v as u8);
        return;
    }
    out.push(first | mask as u8);
    v -= mask;
    while v >= 128 {
        out.push((v & 0x7f) as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_str(out: &mut Vec<u8>, s: &[u8]) {
    let hl = huffman::encoded_len(s);
    if hl < s.len() {
        put_int(out, 0x80, 7, hl);
        huffman::encode(s, out);
    } else {
        put_int(out, 0, 7, s.len());
        out.extend_from_slice(s);
    }
}

pub struct Encoder {
    table: Table,
    index: HashMap<u64, u64, Bh>,
}

impl Encoder {
    pub fn new(peer_table_size: usize) -> Self {
        Encoder {
            table: Table::new(peer_table_size),
            index: HashMap::default(),
        }
    }

    pub fn field(&mut self, out: &mut Vec<u8>, name: &[u8], value: &[u8], mode: Mode) {
        let (names, full) = statics();
        let h = hash2(name, value);
        if mode == Mode::Index {
            if let Some(&i) = full
                .get(&h)
                .filter(|&&i| STATIC[i as usize - 1] == (name, value))
            {
                return put_int(out, 0x80, 7, i as usize);
            }
            if let Some(&id) = self.index.get(&h) {
                let dist = (self.table.inserted - 1 - id) as usize;
                if self.table.get(dist + 1).is_ok_and(|e| e == (name, value)) {
                    return put_int(out, 0x80, 7, 62 + dist);
                }
            }
        }
        let name_idx = names.get(name).copied().unwrap_or(0) as usize;
        let (first, prefix) = match mode {
            Mode::Index => (0x40, 6),
            Mode::NoIndex => (0x00, 4),
            Mode::Never => (0x10, 4),
        };
        put_int(out, first, prefix, name_idx);
        if name_idx == 0 {
            put_str(out, name);
        }
        put_str(out, value);
        if mode == Mode::Index {
            let before = self.table.inserted;
            for g in self.table.insert(name, value, h) {
                if self.index.get(&g.hash) == Some(&g.id) {
                    self.index.remove(&g.hash);
                }
            }
            if self.table.inserted > before {
                self.index.insert(h, before);
            }
        }
    }
}
