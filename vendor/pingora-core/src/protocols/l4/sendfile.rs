// Copyright 2026 Cloudflare, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! `sendfile(2)` from a file to a non-blocking TCP socket.

use std::fs::File;
use std::io;

use tokio::io::Interest;
use tokio::net::TcpStream;

/// Sends up to `length` bytes of `file` starting at `offset` and returns how many were sent, which is less than
/// `length` only when the file ends first. Waits for the socket to be writable whenever its buffer is full.
///
/// The file read happens in the kernel on the calling thread: callers send files whose pages are in the page cache.
pub async fn send_file(
    socket: &TcpStream,
    file: &File,
    offset: u64,
    length: usize,
) -> io::Result<usize> {
    let mut position = libc::off_t::try_from(offset).map_err(io::Error::other)?;
    let mut sent = 0;
    while sent < length {
        let count = socket
            .async_io(Interest::WRITABLE, || {
                nix::sys::sendfile::sendfile(socket, file, Some(&mut position), length - sent)
                    .map_err(io::Error::from)
            })
            .await?;
        if count == 0 {
            break;
        }
        sent += count;
    }
    Ok(sent)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::io::Write;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    async fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        (client, listener.accept().await.unwrap().0)
    }

    fn file_with(bytes: &[u8]) -> File {
        let mut file = tempfile_in_target();
        file.write_all(bytes).unwrap();
        file
    }

    fn tempfile_in_target() -> File {
        let path = std::env::temp_dir().join(format!("sendfile-test-{}", rand::random::<u64>()));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        std::fs::remove_file(path).unwrap();
        file
    }

    #[tokio::test]
    async fn a_range_of_a_file_larger_than_the_socket_buffer_arrives_intact() {
        let data: Vec<u8> = (0..6_000_000u32).map(|i| (i % 251) as u8).collect();
        let file = file_with(&data);
        let (client, server) = pair().await;
        let reader = tokio::spawn(async move {
            let mut client = client;
            let mut received = Vec::new();
            client.read_to_end(&mut received).await.unwrap();
            received
        });
        let sent = send_file(&server, &file, 1000, 5_000_000).await.unwrap();
        assert_eq!(sent, 5_000_000);
        drop(server);
        assert!(reader.await.unwrap() == data[1000..5_001_000]);
    }

    #[tokio::test]
    async fn a_file_that_ends_early_reports_the_bytes_it_had() {
        let file = file_with(&[7; 100]);
        let (client, server) = pair().await;
        let sent = send_file(&server, &file, 40, 1000).await.unwrap();
        assert_eq!(sent, 60);
        drop(server);
        let mut received = Vec::new();
        let mut client = client;
        client.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, vec![7; 60]);
    }
}
