use super::{Count, Invalid, decode, encode, encoded_len};

fn decoded(src: &[u8]) -> Result<Vec<u8>, Invalid> {
    let mut out = Vec::new();
    decode(src, &mut out).map(|()| out)
}

struct Xorshift(u64);

impl Xorshift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn rfc_c4_vectors() {
    let cases: [(&[u8], &[u8]); 3] = [
        (
            b"www.example.com",
            &[
                0xf1, 0xe3, 0xc2, 0xe5, 0xf2, 0x3a, 0x6b, 0xa0, 0xab, 0x90, 0xf4, 0xff,
            ],
        ),
        (b"no-cache", &[0xa8, 0xeb, 0x10, 0x64, 0x9c, 0xbf]),
        (
            b"custom-key",
            &[0x25, 0xa8, 0x49, 0xe9, 0x5b, 0xa9, 0x7d, 0x7f],
        ),
    ];
    for (plain, wire) in cases {
        let mut out = Vec::new();
        encode(plain, &mut out);
        assert_eq!(out, wire);
        assert_eq!(encoded_len(plain), wire.len());
        assert_eq!(decoded(wire).unwrap(), plain);
    }
}

#[test]
fn every_octet_round_trips_alone_and_in_runs() {
    for byte in 0..=255u8 {
        for run in [1, 2, 3, 7, 8, 9] {
            let src = vec![byte; run];
            let mut out = Vec::new();
            encode(&src, &mut out);
            assert_eq!(out.len(), encoded_len(&src));
            assert_eq!(decoded(&out).unwrap(), src, "byte {byte} x{run}");
        }
    }
}

#[test]
fn random_strings_round_trip() {
    let mut rng = Xorshift(0x9e37_79b9_7f4a_7c15);
    for _ in 0..2000 {
        let len = (rng.next() % 300) as usize;
        let ascii = rng.next().is_multiple_of(2);
        let src: Vec<u8> = (0..len)
            .map(|_| {
                if ascii {
                    32 + (rng.next() % 95) as u8
                } else {
                    rng.next() as u8
                }
            })
            .collect();
        let mut out = Vec::new();
        encode(&src, &mut out);
        assert_eq!(decoded(&out).unwrap(), src);
        let mut count = Count(0);
        decode(&out, &mut count).unwrap();
        assert_eq!(count.0, src.len());
    }
}

#[test]
fn empty_input_is_empty_output() {
    assert_eq!(decoded(&[]).unwrap(), b"");
}

#[test]
fn padding_of_eight_bits_or_more_is_rejected() {
    let mut out = Vec::new();
    encode(b"a", &mut out);
    out.push(0xff);
    assert_eq!(decoded(&out), Err(Invalid));
    assert_eq!(decoded(&[0xff]), Err(Invalid));
}

#[test]
fn padding_that_is_not_ones_is_rejected() {
    // 'a' is 00011, padded with 000 instead of 111
    assert_eq!(decoded(&[0b0001_1000]), Err(Invalid));
    assert_eq!(decoded(&[0b0001_1110]), Err(Invalid));
    assert_eq!(decoded(&[0b0001_1111]).unwrap(), b"a");
}

#[test]
fn eos_inside_the_data_is_rejected() {
    // 30 one bits (EOS), then padding
    assert_eq!(decoded(&[0xff, 0xff, 0xff, 0xff]), Err(Invalid));
    assert_eq!(decoded(&[0xff, 0xff, 0xff, 0xfc]), Err(Invalid));
    let mut out = Vec::new();
    encode(b"ab", &mut out);
    out.truncate(1);
    out.extend_from_slice(&[0xff, 0xff, 0xff, 0xff]);
    assert_eq!(decoded(&out), Err(Invalid));
}

#[test]
fn truncated_symbol_is_rejected() {
    // '!' is ten bits, 1111111000: the first byte alone is a partial code with more than seven bits
    assert_eq!(decoded(&[0xfe]), Err(Invalid));
    assert_eq!(decoded(&[0xfe, 0x00]), Err(Invalid));
}

#[test]
fn arbitrary_bytes_never_panic() {
    let mut rng = Xorshift(0x2545_f491_4f6c_dd1d);
    for _ in 0..5000 {
        let len = (rng.next() % 40) as usize;
        let src: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        let _ = decoded(&src);
    }
}
