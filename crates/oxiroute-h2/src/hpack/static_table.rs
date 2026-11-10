//! The static table of RFC 7541 Appendix A and the encoder's name lookup over it.
use super::bytes_eq::eq;

pub(super) const LEN: usize = 61;

const fn e(name: &'static str, value: &'static str) -> (&'static [u8], &'static [u8]) {
    (name.as_bytes(), value.as_bytes())
}

const TABLE: [(&[u8], &[u8]); LEN] = [
    e(":authority", ""),
    e(":method", "GET"),
    e(":method", "POST"),
    e(":path", "/"),
    e(":path", "/index.html"),
    e(":scheme", "http"),
    e(":scheme", "https"),
    e(":status", "200"),
    e(":status", "204"),
    e(":status", "206"),
    e(":status", "304"),
    e(":status", "400"),
    e(":status", "404"),
    e(":status", "500"),
    e("accept-charset", ""),
    e("accept-encoding", "gzip, deflate"),
    e("accept-language", ""),
    e("accept-ranges", ""),
    e("accept", ""),
    e("access-control-allow-origin", ""),
    e("age", ""),
    e("allow", ""),
    e("authorization", ""),
    e("cache-control", ""),
    e("content-disposition", ""),
    e("content-encoding", ""),
    e("content-language", ""),
    e("content-length", ""),
    e("content-location", ""),
    e("content-range", ""),
    e("content-type", ""),
    e("cookie", ""),
    e("date", ""),
    e("etag", ""),
    e("expect", ""),
    e("expires", ""),
    e("from", ""),
    e("host", ""),
    e("if-match", ""),
    e("if-modified-since", ""),
    e("if-none-match", ""),
    e("if-range", ""),
    e("if-unmodified-since", ""),
    e("last-modified", ""),
    e("link", ""),
    e("location", ""),
    e("max-forwards", ""),
    e("proxy-authenticate", ""),
    e("proxy-authorization", ""),
    e("range", ""),
    e("referer", ""),
    e("refresh", ""),
    e("retry-after", ""),
    e("server", ""),
    e("set-cookie", ""),
    e("strict-transport-security", ""),
    e("transfer-encoding", ""),
    e("user-agent", ""),
    e("vary", ""),
    e("via", ""),
    e("www-authenticate", ""),
];

/// Entry at HPACK index `index` (1..=61).
#[inline]
pub(super) fn get(index: usize) -> Option<(&'static [u8], &'static [u8])> {
    TABLE.get(index.checked_sub(1)?).copied()
}

const MIN_NAME: usize = 3;
const MAX_NAME: usize = 27;

/// Perfect hash over length, first and last octet of the 52 distinct names (checked when the slot table is built).
#[inline]
const fn slot(len: usize, first: u8, last: u8) -> usize {
    let mixed = (len as u32 * 58165) ^ (first as u32 * 32421) ^ (last as u32 * 63923);
    (mixed >> 8) as usize & 127
}

/// Slot to the index of the first static entry with that name and how many entries share it (entries of one name
/// are adjacent); index 0 is empty.
static SLOTS: [(u8, u8); 128] = build_slots();

const fn build_slots() -> [(u8, u8); 128] {
    let mut slots = [(0u8, 0u8); 128];
    let mut i = 0;
    while i < LEN {
        let name = TABLE[i].0;
        if i == 0 || !same(TABLE[i - 1].0, name) {
            let s = slot(name.len(), name[0], name[name.len() - 1]);
            assert!(slots[s].0 == 0, "static name hash collision");
            slots[s] = (i as u8 + 1, 0);
        }
        let s = slot(name.len(), name[0], name[name.len() - 1]);
        slots[s].1 += 1;
        i += 1;
    }
    slots
}

const fn same(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Static entries sharing a name.
#[derive(Clone, Copy)]
pub(super) struct Named {
    pub first: usize,
    pub count: usize,
}

impl Named {
    /// Index of the entry with exactly this value, when there is one.
    #[inline]
    pub(super) fn with_value(self, value: &[u8]) -> Option<usize> {
        if value.is_empty() {
            return None;
        }
        (self.first..self.first + self.count).find(|&i| eq(TABLE[i - 1].1, value))
    }
}

#[inline]
pub(super) fn find(name: &[u8]) -> Option<Named> {
    if !(MIN_NAME..=MAX_NAME).contains(&name.len()) {
        return None;
    }
    let (first, count) = SLOTS[slot(name.len(), name[0], name[name.len() - 1])];
    let first = usize::from(first);
    if first == 0 || !eq(TABLE[first - 1].0, name) {
        return None;
    }
    Some(Named {
        first,
        count: usize::from(count),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_static_name_is_found_with_its_entries() {
        for (i, (name, _)) in TABLE.iter().enumerate() {
            let found = find(name).expect("name");
            assert_eq!(TABLE[found.first - 1].0, *name);
            assert!(found.first <= i + 1 && i < found.first + found.count);
            assert!(found.first == 1 || TABLE[found.first - 2].0 != *name);
        }
    }

    #[test]
    fn values_pick_the_right_entry() {
        let status = find(b":status").unwrap();
        assert_eq!((status.first, status.count), (8, 7));
        assert_eq!(status.with_value(b"404"), Some(13));
        assert_eq!(status.with_value(b"201"), None);
        assert_eq!(
            find(b"accept-encoding")
                .unwrap()
                .with_value(b"gzip, deflate"),
            Some(16)
        );
        assert_eq!(find(b"cookie").unwrap().with_value(b""), None);
    }

    #[test]
    fn other_names_are_not_found() {
        for name in [
            &b""[..],
            b"x",
            b"x-request-id",
            b"Accept",
            b"acceptx",
            b"content-typf",
            &[b'a'; 40],
        ] {
            assert!(find(name).is_none(), "{name:?}");
        }
    }

    #[test]
    fn index_bounds() {
        assert!(get(0).is_none() && get(62).is_none());
        assert_eq!(get(61).unwrap().0, b"www-authenticate");
        assert_eq!(get(1).unwrap().0, b":authority");
    }
}
