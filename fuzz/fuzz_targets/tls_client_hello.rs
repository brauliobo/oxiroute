#![no_main]

mod support;

use std::{io, io::Read, sync::Arc};

use libfuzzer_sys::fuzz_target;
use rustls::pki_types::ServerName;
use rustls::server::{ClientHello, ResolvesServerCert};

const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_READ_ITERATIONS: usize = 64;

#[derive(Debug)]
struct NullCertificateResolver;

impl ResolvesServerCert for NullCertificateResolver {
    fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<rustls::sign::CertifiedKey>> {
        let _ = client_hello.server_name();
        let _ = client_hello.alpn();
        None
    }
}

fuzz_target!(|data: &[u8]| {
    let Some(mut data) =
        support::bounded_input(data, MAX_INPUT_BYTES).map(|data| data.into_owned())
    else {
        return;
    };
    if data == b"seed:clienthello" {
        data = valid_client_hello();
    }
    let chunk_size = usize::from(data.first().copied().unwrap_or_default() % 63) + 1;
    let mut reader = FragmentedReader {
        input: &data,
        offset: 0,
        chunk_size,
    };
    let config = rustls::ServerConfig::builder_with_protocol_versions(&[
        &rustls::version::TLS13,
        &rustls::version::TLS12,
    ])
    .with_no_client_auth()
    .with_cert_resolver(Arc::new(NullCertificateResolver));
    let Ok(mut connection) = rustls::ServerConnection::new(Arc::new(config)) else {
        return;
    };

    for _ in 0..MAX_READ_ITERATIONS {
        if reader.offset == reader.input.len() {
            break;
        }
        let Ok(read) = connection.read_tls(&mut reader) else {
            break;
        };
        if read == 0 {
            break;
        }
        if connection.process_new_packets().is_err() {
            break;
        }
    }
});

fn valid_client_hello() -> Vec<u8> {
    let config = rustls::ClientConfig::builder_with_protocol_versions(&[
        &rustls::version::TLS13,
        &rustls::version::TLS12,
    ])
    .with_root_certificates(rustls::RootCertStore::empty())
    .with_no_client_auth();
    let server_name = ServerName::try_from("example.test").expect("static server name");
    let mut connection =
        rustls::ClientConnection::new(Arc::new(config), server_name).expect("client connection");
    let mut output = Vec::new();
    connection
        .write_tls(&mut output)
        .expect("client hello wire");
    output
}

struct FragmentedReader<'a> {
    input: &'a [u8],
    offset: usize,
    chunk_size: usize,
}

impl Read for FragmentedReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let remaining = &self.input[self.offset..];
        if remaining.is_empty() {
            return Ok(0);
        }
        let count = remaining.len().min(output.len()).min(self.chunk_size);
        output[..count].copy_from_slice(&remaining[..count]);
        self.offset += count;
        Ok(count)
    }
}
