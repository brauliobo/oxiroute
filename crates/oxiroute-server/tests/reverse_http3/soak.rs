//! Many connections and streams over mixed fixed, static and proxied routes, with a reload in the middle.
#[allow(clippy::wildcard_imports)]
use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

type Sender = h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>;

const TASKS: usize = 4;
const CONNECTIONS_PER_TASK: usize = 6;
const BATCHES_PER_CONNECTION: usize = 20;
const STREAMS_PER_BATCH: usize = 3;
const TOTAL_BATCHES: usize = TASKS * CONNECTIONS_PER_TASK * BATCHES_PER_CONNECTION;

struct Progress {
    batches: AtomicUsize,
    reloaded: AtomicBool,
    refused: AtomicUsize,
}

fn route(path: &str, action: HttpRouteAction) -> HttpRoute {
    let mut route = reverse_service(action).routes.remove(0);
    route.path = HttpPathSelector::SegmentPrefix { value: path.into() };
    route
}

fn mixed_service(root: &Path, fixed_body: &str) -> HttpService {
    let mut service = static_service(root.to_path_buf());
    let proxy = HttpRouteAction::Proxy {
        upstream_pool: "origin".into(),
        policy: HttpProxyPolicy::default().into(),
    };
    let fixed = HttpRouteAction::FixedResponse {
        status: 200,
        body: fixed_body.into(),
        headers: Vec::new(),
    };
    let files = service.routes.remove(0).action;
    service.routes = vec![
        route("/fixed", fixed),
        route("/proxy", proxy),
        route("/static", files),
    ];
    service.max_request_body_bytes = Some(64 * 1024);
    service
}

/// An origin that answers every request on every connection with its path.
fn spawn_echo_origin() -> SocketAddr {
    let endpoint = quinn::Endpoint::server(origin_server_config(), (Ipv4Addr::LOCALHOST, 0).into())
        .expect("origin endpoint");
    let address = endpoint.local_addr().expect("origin address");
    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            tokio::spawn(serve_origin_connection(incoming));
        }
    });
    address
}

async fn serve_origin_connection(incoming: quinn::Incoming) {
    let Ok(connection) = incoming.await else {
        return;
    };
    let Ok(mut h3) = h3::server::builder()
        .build(h3_quinn::Connection::new(connection))
        .await
    else {
        return;
    };
    while let Ok(Some(resolver)) = h3.accept().await {
        tokio::spawn(async move {
            let Ok((request, mut stream)) = resolver.resolve_request().await else {
                return;
            };
            while let Ok(Some(_)) = stream.recv_data().await {}
            let body = Bytes::from(format!("origin:{}", request.uri().path()));
            let response = Response::builder()
                .header("content-length", body.len().to_string())
                .body(())
                .expect("origin response");
            if stream.send_response(response).await.is_ok() && stream.send_data(body).await.is_ok()
            {
                let _ = stream.finish().await;
            }
        });
    }
}

/// The path and the bodies a correct answer may have.
fn expectation(index: usize) -> (String, Method, Vec<&'static str>) {
    match index % 4 {
        0 => ("/fixed/a".into(), Method::GET, vec!["ok", "ok-v2"]),
        1 => ("/proxy/b".into(), Method::GET, vec!["origin:/proxy/b"]),
        2 => ("/static/a.txt".into(), Method::GET, vec!["static-body"]),
        _ => ("/fixed/head".into(), Method::HEAD, vec![""]),
    }
}

async fn one_request(sender: &mut Sender, index: usize) -> Result<(), String> {
    let (path, method, bodies) = expectation(index);
    let request = Request::builder()
        .method(method)
        .uri(format!("https://example.test{path}"))
        .body(())
        .map_err(|error| error.to_string())?;
    let mut stream = timeout(Duration::from_secs(10), async {
        let mut stream = sender.send_request(request).await?;
        stream.finish().await?;
        Ok::<_, h3::error::StreamError>(stream)
    })
    .await
    .map_err(|_| format!("{path}: request timed out"))?
    .map_err(|error| error.to_string())?;
    let response = timeout(Duration::from_secs(10), stream.recv_response())
        .await
        .map_err(|_| format!("{path}: response timed out"))?
        .map_err(|error| error.to_string())?;
    let body = timeout(Duration::from_secs(10), async {
        let mut body = Vec::new();
        while let Some(mut chunk) = stream.recv_data().await? {
            let length = chunk.remaining();
            body.extend_from_slice(&chunk.copy_to_bytes(length));
        }
        Ok::<_, h3::error::StreamError>(body)
    })
    .await
    .map_err(|_| format!("{path}: body timed out"))?
    .map_err(|error| error.to_string())?;
    assert_eq!(response.status(), StatusCode::OK, "{path}");
    let body = String::from_utf8(body).expect("text body");
    assert!(
        bodies.contains(&body.as_str()),
        "{path}: unexpected body {body:?}"
    );
    Ok(())
}

async fn batch(sender: &mut Sender, first: usize) -> Result<(), String> {
    let requests = (0..STREAMS_PER_BATCH).map(|offset| {
        let mut sender = sender.clone();
        async move { one_request(&mut sender, first + offset).await }
    });
    futures_util::future::join_all(requests)
        .await
        .into_iter()
        .collect()
}

async fn session(endpoint: quinn::Endpoint, address: SocketAddr, progress: Arc<Progress>) {
    for connection_index in 0..CONNECTIONS_PER_TASK {
        let mut done = 0;
        while done < BATCHES_PER_CONNECTION {
            let connection = connect_h3(&endpoint, address).await;
            let (driver, mut sender) =
                h3::client::new(h3_quinn::Connection::new(connection.clone()))
                    .await
                    .expect("H3 client connection");
            let driver = drive_client(driver);
            while done < BATCHES_PER_CONNECTION {
                match batch(&mut sender, connection_index + done).await {
                    Ok(()) => {
                        done += 1;
                        progress.batches.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(error) => {
                        assert!(
                            progress.reloaded.load(Ordering::SeqCst),
                            "request failed without a reload: {error}"
                        );
                        progress.refused.fetch_add(1, Ordering::Relaxed);
                        break;
                    }
                }
            }
            connection.close(quinn::VarInt::from_u32(0), b"session done");
            drop(sender);
            driver.abort();
        }
    }
}

#[tokio::test]
pub(super) async fn reverse_h3_soak_serves_mixed_routes_across_a_reload() {
    let origin_address = spawn_echo_origin();
    let directory = tempfile::tempdir().expect("static directory");
    let root = directory.path().join("public");
    fs::create_dir_all(root.join("static")).expect("static root");
    fs::write(root.join("static/a.txt"), b"static-body").expect("static file");
    let management_address = process_support::reserve_tcp_address();
    let listener_address = reserve_udp_address();
    let key = fixture_support::private_key_fixture("proxy-a-key.pem");
    let mut config = reverse_config(
        listener_address,
        key.path(),
        mixed_service(&root, "ok"),
        vec![h3_pool(origin_address)],
        Some(256),
    );
    config.management = Some(Management {
        bind: management_address,
        ui_dir: None,
        token_file: None,
    });
    let active = config.clone().validate().expect("valid active H3 config");
    let mut server = process_support::ServerProcess::start(&active, Some(TEST_TOKEN));
    server.wait_for_tcp(management_address).await;
    let authorization = format!("Bearer {TEST_TOKEN}");
    let original_revision = active_revision(management_address, &authorization).await;
    let endpoint = client_endpoint().expect("H3 client endpoint");
    let progress = Arc::new(Progress {
        batches: AtomicUsize::new(0),
        reloaded: AtomicBool::new(false),
        refused: AtomicUsize::new(0),
    });
    let tasks = (0..TASKS)
        .map(|_| {
            tokio::spawn(session(
                endpoint.clone(),
                listener_address,
                Arc::clone(&progress),
            ))
        })
        .collect::<Vec<_>>();

    while progress.batches.load(Ordering::Relaxed) < TOTAL_BATCHES / 3 {
        assert!(
            tasks.iter().all(|task| !task.is_finished()),
            "a session ended early"
        );
        sleep(Duration::from_millis(10)).await;
    }
    progress.reloaded.store(true, Ordering::SeqCst);
    config.http_services[0] = mixed_service(&root, "ok-v2");
    process_support::write_config(
        &server.config_path,
        &config.validate().expect("valid candidate H3 config"),
    );
    wait_for_new_revision(management_address, &authorization, &original_revision).await;
    for task in tasks {
        timeout(Duration::from_mins(2), task)
            .await
            .expect("soak session did not finish")
            .expect("soak session task");
    }

    let connection = connect_h3(&endpoint, listener_address).await;
    let (driver, mut sender) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("H3 client connection");
    let driver = drive_client(driver);
    let mut stream = sender
        .send_request(fixed_request("/fixed/after"))
        .await
        .expect("request after reload");
    stream.finish().await.expect("finish request");
    stream.recv_response().await.expect("response after reload");
    assert_eq!(recv_body(&mut stream).await, Bytes::from_static(b"ok-v2"));

    drop(stream);
    drop(sender);
    endpoint.close(quinn::VarInt::from_u32(0), b"test complete");
    driver.await.expect("H3 driver task");
    server.shutdown_gracefully();
}
