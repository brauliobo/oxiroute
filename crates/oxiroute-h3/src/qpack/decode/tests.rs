use super::*;

const WWW_EXAMPLE_COM: [u8; 12] = [
    0xf1, 0xe3, 0xc2, 0xe5, 0xf2, 0x3a, 0x6b, 0xa0, 0xab, 0x90, 0xf4, 0xff,
];

fn lines(block: &[u8]) -> Result<Vec<Line<'_>>, DecodeError> {
    let mut section = section(block)?;
    let mut out = Vec::new();
    while let Some(line) = section.next_line()? {
        out.push(line);
    }
    Ok(out)
}

fn value(line: &Line<'_>) -> Vec<u8> {
    let (Line::NameRef { value, .. } | Line::Literal { value, .. }) = line else {
        panic!("no literal value in {line:?}");
    };
    let mut out = Vec::new();
    value.write_to(&mut out).unwrap();
    out
}

#[test]
fn rfc_9204_b1_literal_with_static_name_reference() {
    let mut block = vec![0x00, 0x00, 0x51, 0x0b];
    block.extend_from_slice(b"/index.html");
    let lines = lines(&block).unwrap();
    assert!(matches!(lines[..], [Line::NameRef { index: 1, .. }]));
    assert_eq!(value(&lines[0]), b"/index.html");
}

#[test]
fn indexed_static_lines() {
    let lines = lines(&[0x00, 0x00, 0xd1, 0xd7, 0xc1, 0xff, 0x23]).unwrap();
    assert_eq!(
        lines,
        [
            Line::Indexed(17),
            Line::Indexed(23),
            Line::Indexed(1),
            Line::Indexed(98)
        ]
    );
}

#[test]
fn huffman_value_and_literal_name() {
    let mut block = vec![0x00, 0x00, 0x50, 0x80 | WWW_EXAMPLE_COM.len() as u8];
    block.extend_from_slice(&WWW_EXAMPLE_COM);
    block.extend_from_slice(&[0x23]);
    block.extend_from_slice(b"x-a");
    block.extend_from_slice(&[0x01, b'v']);
    let lines = lines(&block).unwrap();
    assert_eq!(value(&lines[0]), b"www.example.com");
    let Line::Literal { name, .. } = lines[1] else {
        panic!()
    };
    assert!(!name.huffman);
    assert_eq!(name.bytes, b"x-a");
    assert_eq!(value(&lines[1]), b"v");
}

#[test]
fn huffman_literal_name_flag_is_read_from_bit_three() {
    let mut name = Vec::new();
    oxiroute_h2::hpack::shared::huffman_encode(b"x-key", &mut name);
    let mut block = vec![0x00, 0x00, 0x20 | 0x08 | name.len() as u8];
    block.extend_from_slice(&name);
    block.extend_from_slice(&[0x00]);
    let Line::Literal { name, .. } = lines(&block).unwrap()[0] else {
        panic!()
    };
    assert!(name.huffman);
}

#[test]
fn sections_that_need_the_dynamic_table_are_rejected() {
    for block in [
        &[0x01, 0x00][..],         // Required Insert Count 1
        &[0x00, 0x80],             // negative base
        &[0x00, 0x00, 0x80],       // indexed, dynamic
        &[0x00, 0x00, 0x10],       // indexed post-base
        &[0x00, 0x00, 0x40, 0x00], // literal with dynamic name reference
        &[0x00, 0x00, 0x00, 0x00], // literal with post-base name reference
    ] {
        assert!(lines(block).is_err(), "{block:02x?}");
    }
}

#[test]
fn out_of_range_static_indices_and_truncations_are_rejected() {
    assert!(lines(&[0x00, 0x00, 0xff, 0x24]).is_err()); // index 99
    assert!(lines(&[0x00, 0x00, 0x5f, 0x54, 0x00]).is_err()); // name index 99
    assert!(lines(&[0x00]).is_err());
    assert!(lines(&[]).is_err());
    assert!(lines(&[0x00, 0x00, 0x51]).is_err());
    assert!(lines(&[0x00, 0x00, 0x51, 0x05, b'a']).is_err());
    assert!(lines(&[0x00, 0x00, 0x23, b'a', b'b']).is_err());
}

#[test]
fn invalid_huffman_data_fails_when_written() {
    let mut out = Vec::new();
    let eos = Str {
        bytes: &[0xff, 0xff, 0xff, 0xff],
        huffman: true,
    };
    assert!(eos.write_to(&mut out).is_err());
}

#[test]
fn delta_base_may_span_octets() {
    assert!(lines(&[0x00, 0x7f, 0x80, 0x01, 0xd1]).is_ok());
    assert!(lines(&[0x00, 0x7f, 0x80]).is_err());
}
