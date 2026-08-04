#![no_main]

mod support;

use libfuzzer_sys::fuzz_target;
use oxiroute_import::{SourceFile, SourceId, apache, haproxy, nginx, varnish};

const MAX_INPUT_BYTES: usize = 128 * 1024;

fuzz_target!(|data: &[u8]| {
    let Some(data) = support::bounded_input(data, MAX_INPUT_BYTES) else {
        return;
    };
    let data = data.as_ref();
    let (parser, source_bytes) = select_parser(data);

    let source = SourceFile::new(
        SourceId::new(1),
        "fuzz-native-source",
        source_bytes.to_vec(),
    );
    match parser {
        Parser::Nginx => {
            let _ = nginx::parse(&source);
        }
        Parser::Haproxy => {
            let _ = haproxy::parse(&source);
        }
        Parser::Apache => {
            let _ = apache::parse(&source);
        }
        #[cfg(unix)]
        Parser::Squid => {
            let _ = oxiroute_import::squid::parse(&source);
        }
        Parser::Varnish => {
            let _ = varnish::parse(&source);
        }
    }
});

#[derive(Clone, Copy, Eq, PartialEq)]
enum Parser {
    Nginx,
    Haproxy,
    Apache,
    #[cfg(unix)]
    Squid,
    Varnish,
}

fn select_parser(data: &[u8]) -> (Parser, &[u8]) {
    for (prefix, parser) in [
        (b"nginx:".as_slice(), Parser::Nginx),
        (b"haproxy:".as_slice(), Parser::Haproxy),
        (b"apache:".as_slice(), Parser::Apache),
        (b"varnish:".as_slice(), Parser::Varnish),
    ] {
        if let Some(source) = support::strip_prefix(data, prefix) {
            return (parser, source);
        }
    }

    #[cfg(unix)]
    let parsers = [
        Parser::Nginx,
        Parser::Haproxy,
        Parser::Apache,
        Parser::Squid,
        Parser::Varnish,
    ];
    #[cfg(not(unix))]
    let parsers = [
        Parser::Nginx,
        Parser::Haproxy,
        Parser::Apache,
        Parser::Varnish,
    ];
    let selector = usize::from(data.first().copied().unwrap_or_default()) % parsers.len();
    (parsers[selector], data.get(1..).unwrap_or_default())
}
