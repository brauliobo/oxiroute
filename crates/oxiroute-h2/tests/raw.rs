//! Raw-frame tests: drive the server with hand-built frames and assert the observable reaction.
mod support;
use oxiroute_h2::server::Config;
use support::*;
use tokio::io::duplex;

async fn start(
    config: Config,
) -> (
    Wire,
    tokio::task::JoinHandle<Result<(), oxiroute_h2::Error>>,
) {
    let (client, server) = duplex(1 << 20);
    let task = tokio::spawn(serve_ours(server, config));
    let mut wire = Wire::new(client);
    wire.start().await;
    (wire, task)
}

#[tokio::test(start_paused = true)]
async fn a_get_is_answered() {
    let (mut wire, _task) = start(Config::default()).await;
    let mut c = Client::new();
    wire.send(&c.get(1, "/")).await;
    let frames = wire.settle().await;
    let r = wire.reactions(&frames);
    assert_eq!(r, vec![Reaction::Status(1, 200)]);
}
