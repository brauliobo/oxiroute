//! The server under test of the raw-frame tests: an application behind an in-memory pipe, driven with a `Wire`.
#![allow(dead_code)]
use bytes::Bytes;
use oxiroute_h2::server::{Config, Server};
use oxiroute_h2::{Error, Reason};
use tokio::io::duplex;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::support::*;

pub const PROTOCOL: u32 = 1;
pub const FLOW_CONTROL: u32 = 3;
pub const SETTINGS_TIMEOUT: u32 = 4;
pub const STREAM_CLOSED: u32 = 5;
pub const FRAME_SIZE: u32 = 6;
pub const REFUSED: u32 = 7;
pub const COMPRESSION: u32 = 9;
pub const CALM: u32 = 11;

/// What the application does with an accepted request.
#[derive(Clone, Copy)]
pub enum Mode {
    /// 200 with a two-octet body, inline.
    Respond,
    /// Keeps the stream open without answering.
    Hold,
    /// Drops the stream at once.
    Drop,
    /// Reads the body and sends it back.
    Echo,
    /// 200 with a body of this many octets.
    Body(usize),
}

pub enum Cmd {
    Graceful,
    Abrupt(Reason),
}

pub struct Rig {
    pub wire: Wire,
    pub task: JoinHandle<Result<(), Error>>,
    pub ctl: mpsc::UnboundedSender<Cmd>,
}

async fn serve(
    io: tokio::io::DuplexStream,
    config: Config,
    mode: Mode,
    mut rx: mpsc::UnboundedReceiver<Cmd>,
) -> Result<(), Error> {
    let mut server = Server::handshake(io, config).await?;
    let mut held = Vec::new();
    loop {
        tokio::select! {
            r = server.accept() => {
                let Some(r) = r else { return Ok(()) };
                let (_parts, mut stream) = r?;
                match mode {
                    Mode::Respond => {
                        let (status, headers) = response(200, &[]);
                        stream.send_response(status, headers, false)?;
                        write_body(&mut stream, Bytes::from_static(b"ok"), true).await?;
                    }
                    Mode::Hold => held.push(stream),
                    Mode::Drop => drop(stream),
                    Mode::Echo => {
                        tokio::spawn(async move {
                            let Ok(body) = read_body(&mut stream).await else { return };
                            let (status, headers) = response(200, &[]);
                            stream.send_response(status, headers, false).unwrap();
                            write_body(&mut stream, Bytes::from(body), true).await.unwrap();
                        });
                    }
                    Mode::Body(n) => {
                        tokio::spawn(async move {
                            let (status, headers) = response(200, &[]);
                            stream.send_response(status, headers, false).unwrap();
                            let body: Vec<u8> = (0..n).map(|i| i as u8).collect();
                            write_body(&mut stream, Bytes::from(body), true).await.unwrap();
                        });
                    }
                }
            }
            Some(cmd) = rx.recv() => match cmd {
                Cmd::Graceful => server.graceful_shutdown(),
                Cmd::Abrupt(reason) => server.abrupt_shutdown(reason),
            },
        }
    }
}

/// Server running `mode` behind a pipe of `buf` octets per direction; the client has not sent anything yet.
pub fn rig(config: Config, mode: Mode, buf: usize) -> Rig {
    let (client, server) = duplex(buf);
    let (ctl, rx) = mpsc::unbounded_channel();
    Rig {
        wire: Wire::new(client),
        task: tokio::spawn(serve(server, config, mode, rx)),
        ctl,
    }
}

/// Server with a completed preface and SETTINGS exchange.
pub async fn start(config: Config, mode: Mode) -> Rig {
    let mut r = rig(config, mode, 1 << 20);
    r.wire.start().await;
    r
}

impl Rig {
    /// Sends `bytes`, waits for quiescence and returns the reactions.
    pub async fn exchange(&mut self, bytes: &[u8]) -> Vec<Reaction> {
        self.wire.send(bytes).await;
        self.settle().await
    }

    pub async fn settle(&mut self) -> Vec<Reaction> {
        let frames = self.wire.settle().await;
        self.wire.reactions(&frames)
    }
}

pub fn cfg(f: impl FnOnce(&mut Config)) -> Config {
    let mut c = Config::default();
    f(&mut c);
    c
}

pub fn concat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}
