//! The running daemon, its helper endpoints and the recorded cases.

use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::time::sleep;

use crate::{
    client::{self, Req, Session},
    config::{self, Addresses, Layout, MANAGEMENT_TOKEN},
    golden::{self, Case, Tolerance},
    origin::{Origin, Refuser},
    process_support::{self, ServerProcess},
    stats,
};

/// Lets the origin log and the server's own bookkeeping catch up with a response the client has read.
const SETTLE: Duration = Duration::from_millis(30);

pub struct Context {
    server: Option<ServerProcess>,
    pub layout: Layout,
    pub addresses: Addresses,
    pub origin: Origin,
    pub refuser: Refuser,
    pub endpoint: quinn::Endpoint,
    cases: Vec<Case>,
    stats: Map<String, Value>,
}

/// `fixed/get-ok` is requested as `get-ok.fixed.test`, so the access log lines of a case can be told apart.
pub fn host(id: &str) -> String {
    let (area, name) = id.split_once('/').expect("case id is area/name");
    format!("{name}.{area}.test")
}

impl Context {
    pub async fn start() -> Self {
        let origin = Origin::start();
        let refuser = Refuser::start();
        let addresses = Addresses {
            listener: process_support::ports::unused_address(),
            management: process_support::ports::unused_address(),
            origin: origin.address,
            refuser: refuser.address,
        };
        golden::set_scrub(vec![
            (addresses.origin.to_string(), "<origin>"),
            (addresses.refuser.to_string(), "<refuser>"),
        ]);
        let layout = Layout::create();
        let configuration = config::build(&layout, &addresses);
        let mut server = ServerProcess::start(&configuration, Some(MANAGEMENT_TOKEN));
        server.wait_for_tcp(addresses.management).await;
        Self {
            server: Some(server),
            layout,
            addresses,
            origin,
            refuser,
            endpoint: client::client_endpoint(),
            cases: Vec::new(),
            stats: Map::new(),
        }
    }

    pub async fn session(&self) -> Session {
        Session::connect(&self.endpoint, self.addresses.listener).await
    }

    /// One request and everything that happened because of it.
    pub async fn step(&self, session: &mut Session, host: &str, req: &Req) -> Value {
        let outcome = session.exchange(host, req).await;
        self.observe(&req.describe(host), outcome).await
    }

    /// An outcome together with what the origin received and how many connections were refused meanwhile.
    pub async fn observe(&self, request: &str, outcome: Value) -> Value {
        sleep(SETTLE).await;
        let mut step = json!({ "request": request, "outcome": outcome });
        let origin = self.origin.take();
        if !origin.is_empty() {
            step["origin"] = Value::Array(origin);
        }
        let refused = self.refuser.take();
        if refused > 0 {
            step["refused"] = json!(refused);
        }
        step
    }

    /// The requests in order on one connection.
    pub async fn run(&mut self, id: &str, requests: Vec<Req>) {
        self.run_with(id, requests, None).await;
    }

    pub async fn run_tolerant(&mut self, id: &str, requests: Vec<Req>, tolerance: Tolerance) {
        self.run_with(id, requests, Some(tolerance)).await;
    }

    async fn run_with(&mut self, id: &str, requests: Vec<Req>, tolerance: Option<Tolerance>) {
        let host = host(id);
        let mut hosts = vec![host.clone()];
        hosts.extend(requests.iter().filter_map(|request| request.host.clone()));
        let mut session = self.session().await;
        let mut steps = Vec::new();
        for request in &requests {
            steps.push(self.step(&mut session, &host, request).await);
        }
        steps.last_mut().expect("a case has steps")["connection_after"] = session.state().await;
        session.close();
        self.record(id, hosts, steps, tolerance);
    }

    pub fn record(
        &mut self,
        id: &str,
        hosts: Vec<String>,
        steps: Vec<Value>,
        tolerance: Option<Tolerance>,
    ) {
        assert!(
            self.cases.iter().all(|case| case.id != id),
            "duplicate case {id}"
        );
        eprintln!("case {id}");
        let tolerance = tolerance.map(|tolerance| tolerance.with_steps(&steps));
        self.cases.push(Case {
            id: id.into(),
            hosts,
            steps,
            tolerance,
        });
    }

    /// Records one step of a connection-level case that is not an HTTP exchange.
    pub fn record_step(
        &mut self,
        id: &str,
        request: &str,
        outcome: Value,
        tolerance: Option<Tolerance>,
    ) {
        let step = json!({ "request": request, "outcome": outcome });
        self.record(id, vec![host(id)], vec![step], tolerance);
    }

    pub async fn wait_idle(&self) {
        self.wait_idle_below(1).await;
    }

    /// Waits until the listener holds fewer than `limit` connections.
    pub async fn wait_idle_below(&self, limit: u64) {
        for _ in 0..200 {
            if stats::active_connections(self.addresses.management).await < limit {
                return;
            }
            sleep(Duration::from_millis(50)).await;
        }
        panic!("the listener kept connections open");
    }

    pub async fn checkpoint(&mut self, name: &str) {
        self.wait_idle().await;
        let counters = stats::settled(self.addresses.management).await;
        self.stats.insert(name.into(), counters);
    }

    pub async fn finish(mut self) -> Value {
        self.wait_idle().await;
        let log = stats::settled_access_log(&self.layout.access_log).await;
        let server = self.server.take().expect("running daemon");
        tokio::task::spawn_blocking(move || server.shutdown_gracefully())
            .await
            .expect("graceful shutdown");
        golden::observation(&self.cases, &log, self.stats)
    }
}
