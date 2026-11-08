use serde_json::{Map, Value, json};

use super::{canonical, decode_generic, render};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next() % bound as u64).unwrap()
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

const NAMES: &[&str] = &[
    "name",
    "listen",
    "x.y",
    "a-b",
    "_a",
    "1abc",
    "true",
    "null",
    "inf",
    "has space",
    "é",
    "",
    "-",
    "a/b",
    "a#b",
    "q\"q",
    "tab\t",
    "-1",
    ".5",
    "+a",
    "r",
    "version",
];
const STRINGS: &[&str] = &[
    "",
    "plain",
    "with \"quote\"",
    "back\\slash",
    "new\nline",
    "tab\t",
    "uni é ü 日本",
    "\u{1}",
    "\u{7f}",
    "\u{85}",
    "\u{2028}",
    "\u{202e}",
    "\u{feff}",
    "#hash",
    "a b  c",
    "{}",
    "//x",
    "/* x */",
    "\u{8}\u{c}\r",
];
const NUMBERS: &[&str] = &[
    "0",
    "-0",
    "7",
    "-7",
    "007",
    "123456789012",
    "18446744073709551615",
    "-9223372036854775808",
    "1.5",
    "-0.25",
    "1e5",
    "1.0e-7",
    "1E+3",
    "0.0",
    "1e999",
    "1e",
    "1.",
    ".5",
    "+5",
    "1_000",
    "0x10",
    "99999999999999999999",
];

fn scalar(rng: &mut Rng) -> Value {
    match rng.below(5) {
        0 => Value::Bool(rng.below(2) == 0),
        1 => Value::Null,
        2 => Value::String(rng.pick(STRINGS).to_owned()),
        3 => serde_json::from_str(rng.pick(NUMBERS)).unwrap_or(Value::Null),
        _ => json!(rng.below(100_000)),
    }
}

fn value(rng: &mut Rng, depth: usize) -> Value {
    match if depth > 4 { 0 } else { rng.below(3) } {
        1 => Value::Object(object(rng, depth + 1)),
        2 => Value::Array((0..rng.below(4)).map(|_| value(rng, depth + 1)).collect()),
        _ => scalar(rng),
    }
}

fn object(rng: &mut Rng, depth: usize) -> Map<String, Value> {
    (0..rng.below(5))
        .map(|_| (rng.pick(NAMES).to_owned(), value(rng, depth)))
        .collect()
}

fn agree(source: &str) -> bool {
    match canonical::decode(source) {
        None => false,
        Some(fast) => {
            assert_eq!(
                fast,
                decode_generic(source).expect("accepted by the fast reader"),
                "{source:?}"
            );
            true
        }
    }
}

#[test]
fn canonical_reader_matches_the_generic_parser_on_rendered_documents() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut accepted = 0;
    for _ in 0..4000 {
        let source = render(&Value::Object(object(&mut rng, 0))).unwrap();
        accepted += usize::from(agree(&source));
    }
    assert!(
        accepted > 1000,
        "the fast reader accepted only {accepted} of 4000 documents"
    );
}

#[test]
fn canonical_reader_never_accepts_what_the_generic_parser_rejects_or_reads_differently() {
    let mut rng = Rng(0xdead_beef_cafe_f00d);
    let junk = [
        b' ', b'\n', b'\t', b'"', b'\\', b'(', b')', b'{', b'}', b'#', b'/', b';', b'=', b'\r',
        0xc3, b'-', b'1',
    ];
    for _ in 0..3000 {
        let mut bytes = render(&Value::Object(object(&mut rng, 0)))
            .unwrap()
            .into_bytes();
        for _ in 0..=rng.below(3) {
            if bytes.is_empty() {
                break;
            }
            let at = rng.below(bytes.len());
            match rng.below(3) {
                0 => bytes[at] = junk[rng.below(junk.len())],
                1 => bytes.insert(at, junk[rng.below(junk.len())]),
                _ => {
                    bytes.remove(at);
                }
            }
        }
        if let Ok(source) = String::from_utf8(bytes)
            && let Some(fast) = canonical::decode(&source)
        {
            assert_eq!(
                Ok(fast),
                decode_generic(&source).map_err(|_| ()),
                "{source:?}"
            );
        }
    }
}

#[test]
fn canonical_reader_declines_non_canonical_documents() {
    for source in [
        "a 1 // comment\n",
        "a 1;b 2\n",
        "a 1\r\nb 2\r\n",
        "a  1\n\tb 2\n",
        "a r#\"raw\"#\n",
        "a \"\"\"\nmulti\n\"\"\"\n",
        "a \"\\u{41}\"\n",
        "a key=1\n",
        "a (decimal)1\n",
        "(object)a {\n  b 1\n  b 2\n}\n",
        "(array)a {\n  b 1\n}\n",
        "(object)a 1 {\n}\n",
        "a #inf\n",
        "a 1e999\n",
        "}\n",
        "(object)a {\n",
    ] {
        assert!(!agree(source), "{source:?} was accepted");
    }
}

#[test]
fn canonical_reader_reads_the_rendered_shape() {
    let source = "(array)items {\n  (object)- {\n    on #true\n    ratio 1.5\n  }\n  - \"x\\n\"\n}\nversion 1\n";
    assert_eq!(
        canonical::decode(source),
        Some(json!({"items": [{"on": true, "ratio": 1.5}, "x\n"], "version": 1}))
    );
}

/// The renderer as it was before the direct writer: one allocation per name, indent and scalar.
fn reference_render(object: &Map<String, Value>, depth: usize, out: &mut String) {
    let mut entries = object.iter().collect::<Vec<_>>();
    entries.sort_unstable_by_key(|(left, _)| *left);
    for (key, value) in entries {
        reference_node(key, value, depth, out);
    }
}

fn reference_node(name: &str, value: &Value, depth: usize, out: &mut String) {
    use std::fmt::Write as _;
    let indent = "  ".repeat(depth);
    let name = kdl::KdlIdentifier::from(name).to_string();
    match value {
        Value::Object(object) => {
            writeln!(out, "{indent}(object){name} {{").unwrap();
            reference_render(object, depth + 1, out);
            writeln!(out, "{indent}}}").unwrap();
        }
        Value::Array(values) => {
            writeln!(out, "{indent}(array){name} {{").unwrap();
            for value in values {
                reference_node("-", value, depth + 1, out);
            }
            writeln!(out, "{indent}}}").unwrap();
        }
        Value::Null => writeln!(out, "{indent}{name} {}", kdl::KdlValue::Null).unwrap(),
        Value::Bool(value) => {
            writeln!(out, "{indent}{name} {}", kdl::KdlValue::Bool(*value)).unwrap();
        }
        Value::Number(value) => {
            let scalar = value.as_i64().map_or_else(
                || {
                    value.as_u64().map_or_else(
                        || kdl::KdlValue::Float(value.as_f64().unwrap()).to_string(),
                        |value| kdl::KdlValue::Integer(i128::from(value)).to_string(),
                    )
                },
                |value| kdl::KdlValue::Integer(i128::from(value)).to_string(),
            );
            writeln!(out, "{indent}{name} {scalar}").unwrap();
        }
        Value::String(value) => {
            writeln!(
                out,
                "{indent}{name} {}",
                serde_json::to_string(value).unwrap()
            )
            .unwrap();
        }
    }
}

#[test]
fn direct_renderer_writes_exactly_what_the_allocating_renderer_wrote() {
    let mut rng = Rng(0x0123_4567_89ab_cdef);
    for _ in 0..2000 {
        let root = object(&mut rng, 0);
        let mut expected = String::new();
        reference_render(&root, 0, &mut expected);
        assert_eq!(render(&Value::Object(root)).unwrap(), expected);
    }
}
