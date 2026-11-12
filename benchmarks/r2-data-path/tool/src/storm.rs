//! `r2tool storm --target ADDR --rate N [--duration S] [--tls-version 1.3]`: an open-loop storm of TLS handshakes,
//! N per second for S seconds (until killed when `--duration` is left out), to measure what new connections cost
//! the connections that are already being served. Prints `sent`, `completed` and `failed` at the end.

use std::{
    net::SocketAddr,
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use rustls_pki_types::ServerName;
use tokio::{net::TcpStream, time::MissedTickBehavior};

use crate::{Flags, flag, load::tls_connector};

pub fn run(flags: &Flags) -> ExitCode {
    let target: SocketAddr = flag(flags, "target", "127.0.0.1:0".to_owned())
        .parse()
        .expect("--target address");
    let rate: u64 = flag(flags, "rate", 1000);
    let duration: f64 = flag(flags, "duration", 0.0);
    let version: String = flag(flags, "tls-version", "1.3".to_owned());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async move {
        let connector = tls_connector(b"h2", &version, false, "default", None);
        let (sent, completed, failed) = (
            AtomicU64::new(0),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        );
        let start = Instant::now();
        let mut tick = tokio::time::interval(Duration::from_nanos(1_000_000_000 / rate.max(1)));
        tick.set_missed_tick_behavior(MissedTickBehavior::Burst);
        while duration == 0.0 || start.elapsed().as_secs_f64() < duration {
            tick.tick().await;
            sent.fetch_add(1, Ordering::Relaxed);
            let (connector, completed, failed) =
                (connector.clone(), completed.clone(), failed.clone());
            tokio::spawn(async move {
                let handshake = async {
                    let tcp = TcpStream::connect(target).await?;
                    let name = ServerName::try_from("bench.example.com").expect("server name");
                    connector.connect(name, tcp).await.map(drop)
                };
                let counter = if handshake.await.is_ok() {
                    &completed
                } else {
                    &failed
                };
                counter.fetch_add(1, Ordering::Relaxed);
            });
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        println!("sent={}", sent.load(Ordering::Relaxed));
        println!("completed={}", completed.load(Ordering::Relaxed));
        println!("failed={}", failed.load(Ordering::Relaxed));
    });
    ExitCode::SUCCESS
}
