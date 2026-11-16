use oxiroute_config::Stats;
use tempfile::tempdir;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    time::{Duration, timeout},
};

use crate::{
    common::{exchange, exchange_head, forward_cache_config, read_request_head},
    config_support::load_lua,
    fixture_support, process_support,
};

#[tokio::test]
async fn cache_serves_a_second_absolute_form_get_without_reaching_the_origin() {
    let origin = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let origin_address = origin.local_addr().expect("origin address");
    let origin_task = tokio::spawn(async move {
        let (mut stream, _) = origin.accept().await.expect("origin accept");
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).await.expect("origin read");
            assert_ne!(read, 0, "origin request ended before headers");
            request.extend_from_slice(&buffer[..read]);
        }
        assert!(request.starts_with(b"GET /cached HTTP/1.1\r\n"));
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nCache-Control: max-age=60\r\nContent-Length: 6\r\nConnection: close\r\n\r\ncached",
            )
            .await
            .expect("origin response");
    });

    let proxy_address = process_support::reserve_tcp_address();
    let config = load_lua(&format!(
        r#"return {{
  version = 1,
  listeners = {{ {{
    name = "forward",
    bind = {{ type = "socket", address = "{proxy_address}" }},
    protocol = "forward_http1",
    service = "forward",
  }} }},
  cache_stores = {{ {{ name = "memory", type = "memory" }} }},
  forward_proxy_services = {{ {{
    name = "forward",
    tls_required = false,
    destination_policy = {{ deny_private = false }},
    cache = {{ store = "memory" }},
  }} }},
}}"#,
    ))
    .expect("forward cache config");
    let mut server = process_support::ServerProcess::start(&config, None);
    server.wait_for_tcp(proxy_address).await;

    let first = exchange(
        proxy_address,
        format!(
            "GET http://{origin_address}/cached HTTP/1.1\r\nHost: origin\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(first.starts_with(b"HTTP/1.1 200"));
    assert!(first.ends_with(b"cached"));

    tokio::time::sleep(Duration::from_millis(20)).await;
    let second = exchange(
        proxy_address,
        format!(
            "GET http://{origin_address}/cached HTTP/1.1\r\nHost: origin\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(second.starts_with(b"HTTP/1.1 200"));
    assert!(second.ends_with(b"cached"));

    origin_task.await.expect("origin task");
    server.shutdown();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn authenticated_absolute_form_bypasses_anonymous_forward_cache() {
    let origin = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let origin_address = origin.local_addr().expect("origin address");
    let origin_task = tokio::spawn(async move {
        let mut buffer = [0; 1024];
        let (mut anonymous, _) = origin.accept().await.expect("anonymous origin accept");
        let mut request = Vec::new();
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = anonymous
                .read(&mut buffer)
                .await
                .expect("anonymous origin read");
            assert_ne!(read, 0, "anonymous request ended before headers");
            request.extend_from_slice(&buffer[..read]);
        }
        assert!(request.starts_with(b"GET /boundary HTTP/1.1\r\n"));
        anonymous
            .write_all(
                b"HTTP/1.1 200 OK\r\nCache-Control: max-age=60\r\nContent-Length: 6\r\nConnection: close\r\n\r\ncached",
            )
            .await
            .expect("anonymous origin response");

        let (mut authenticated, _) = origin.accept().await.expect("authenticated origin accept");
        let mut request = Vec::new();
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = authenticated
                .read(&mut buffer)
                .await
                .expect("authenticated origin read");
            assert_ne!(read, 0, "authenticated request ended before headers");
            request.extend_from_slice(&buffer[..read]);
        }
        assert!(request.starts_with(b"GET /boundary HTTP/1.1\r\n"));
        authenticated
            .write_all(
                b"HTTP/1.1 200 OK\r\nCache-Control: max-age=60\r\nContent-Length: 7\r\nConnection: close\r\n\r\nprivate",
            )
            .await
            .expect("authenticated origin response");
    });

    let token_directory = tempdir().expect("forward token directory");
    let token = "0123456789abcdefghijklmnopqrstuv";
    let token_path = fixture_support::write_file_with_mode(
        token_directory.path(),
        "proxy.token",
        format!("{token}\n").as_bytes(),
        0o600,
    );
    let proxy_address = process_support::reserve_tcp_address();
    let config = load_lua(&format!(
        r#"return {{
  version = 1,
  listeners = {{ {{
    name = "forward",
    bind = {{ type = "socket", address = "{proxy_address}" }},
    protocol = "forward_http1",
    service = "forward",
  }} }},
  cache_stores = {{ {{ name = "memory", type = "memory" }} }},
  forward_proxy_services = {{ {{
    name = "forward",
    tls_required = false,
    auth = {{ type = "bearer_token_file", token_file_path = "{}" }},
    access_policy = {{
      rules = {{
        {{ action = "allow", conditions = {{
          {{ type = "methods", methods = {{ "GET" }} }},
          {{ type = "authenticated", negated = true }},
        }} }},
        {{ action = "allow", conditions = {{ {{ type = "authenticated" }} }} }},
      }},
      default_action = "deny",
    }},
    destination_policy = {{ deny_private = false }},
    cache = {{ store = "memory" }},
  }} }},
}}"#,
        token_path.display()
    ))
    .expect("forward cache boundary config");
    let mut server = process_support::ServerProcess::start(&config, None);
    server.wait_for_tcp(proxy_address).await;

    let anonymous = exchange(
        proxy_address,
        format!(
            "GET http://{origin_address}/boundary HTTP/1.1\r\nHost: origin\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(anonymous.starts_with(b"HTTP/1.1 200"));
    assert!(anonymous.ends_with(b"cached"));

    tokio::time::sleep(Duration::from_millis(20)).await;
    let authenticated = exchange(
        proxy_address,
        format!(
            "GET http://{origin_address}/boundary HTTP/1.1\r\nHost: origin\r\nProxy-Authorization: Bearer {token}\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(authenticated.starts_with(b"HTTP/1.1 200"));
    assert!(authenticated.ends_with(b"private"));

    let cached = exchange(
        proxy_address,
        format!(
            "GET http://{origin_address}/boundary HTTP/1.1\r\nHost: origin\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(cached.starts_with(b"HTTP/1.1 200"));
    assert!(cached.ends_with(b"cached"));

    timeout(Duration::from_secs(30), origin_task)
        .await
        .expect("origin boundary task timeout")
        .expect("origin boundary task");
    server.shutdown();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn cache_collapses_gets_preserves_head_only_if_cached_and_exact_metrics() {
    let origin = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let origin_address = origin.local_addr().expect("origin address");
    let origin_task = tokio::spawn(async move {
        let mut buffer = [0; 1024];
        let (mut leader, _) = origin.accept().await.expect("collapsed leader accept");
        let request = read_request_head(&mut leader, &mut buffer).await;
        assert!(request.starts_with(b"GET /collapse HTTP/1.1\r\n"));
        tokio::time::sleep(Duration::from_millis(100)).await;
        leader
            .write_all(
                b"HTTP/1.1 200 OK\r\nCache-Control: max-age=60\r\nContent-Length: 9\r\nConnection: close\r\n\r\ncollapsed",
            )
            .await
            .expect("collapsed response");

        for _ in 0..2 {
            let (mut head, _) = origin.accept().await.expect("HEAD origin accept");
            let request = read_request_head(&mut head, &mut buffer).await;
            assert!(request.starts_with(b"HEAD /head HTTP/1.1\r\n"));
            head.write_all(
                b"HTTP/1.1 200 OK\r\nCache-Control: max-age=60\r\nContent-Length: 4\r\nConnection: close\r\n\r\n",
            )
            .await
            .expect("HEAD origin response");
        }
    });

    let proxy_address = process_support::reserve_tcp_address();
    let stats_address = process_support::reserve_tcp_address();
    let mut config = forward_cache_config(proxy_address);
    config.stats = Some(Stats {
        binds: vec![stats_address],
        admin_token_file: None,
        pages: Vec::new(),
    });
    let config = config.validate().expect("valid forward proxy config");
    let mut server = process_support::ServerProcess::start(&config, None);
    server.wait_for_tcp(proxy_address).await;
    server.wait_for_tcp(stats_address).await;

    let request = format!(
        "GET http://{origin_address}/collapse HTTP/1.1\r\nHost: origin\r\nConnection: close\r\n\r\n"
    );
    let leader_request = request.clone().into_bytes();
    let leader = tokio::spawn(async move { exchange(proxy_address, &leader_request).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let follower_request = request.into_bytes();
    let follower = tokio::spawn(async move { exchange(proxy_address, &follower_request).await });
    let leader = leader.await.expect("collapsed leader task");
    let follower = follower.await.expect("collapsed follower task");
    assert!(leader.starts_with(b"HTTP/1.1 200"));
    assert!(follower.starts_with(b"HTTP/1.1 200"));
    assert!(leader.ends_with(b"collapsed"));
    assert!(follower.ends_with(b"collapsed"));

    let only_if_cached = exchange(
        proxy_address,
        format!(
            "GET http://{origin_address}/missing HTTP/1.1\r\nHost: origin\r\nCache-Control: only-if-cached\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(only_if_cached.starts_with(b"HTTP/1.1 504"));

    for _ in 0..2 {
        let head = exchange(
            proxy_address,
            format!(
                "HEAD http://{origin_address}/head HTTP/1.1\r\nHost: origin\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await;
        assert!(head.starts_with(b"HTTP/1.1 200"));
        assert!(head.ends_with(b"\r\n\r\n"));
    }

    tokio::time::sleep(Duration::from_millis(20)).await;
    let metrics = exchange(
        stats_address,
        b"GET /metrics HTTP/1.1\r\nHost: stats\r\nConnection: close\r\n\r\n",
    )
    .await;
    let metrics = String::from_utf8(metrics).expect("metrics UTF-8");
    assert!(metrics.contains("oxiroute_http_cache_hits_total{listener=\"forward\"} 1"));
    assert!(metrics.contains("oxiroute_http_cache_misses_total{listener=\"forward\"} 5"));
    assert!(metrics.contains("oxiroute_http_cache_admissions_total{listener=\"forward\"} 1"));
    assert!(metrics.contains("oxiroute_http_cache_evictions_total{listener=\"forward\"} 0"));

    origin_task.await.expect("cache lifecycle origin task");
    server.shutdown();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn cache_revalidates_304_and_serves_stale_only_for_upstream_failures() {
    let origin = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let origin_address = origin.local_addr().expect("origin address");
    let origin_task = tokio::spawn(async move {
        let mut buffer = [0; 1024];
        let (mut initial, _) = origin.accept().await.expect("initial origin accept");
        let request = read_request_head(&mut initial, &mut buffer).await;
        assert!(request.starts_with(b"GET /stale HTTP/1.1\r\n"));
        initial
            .write_all(
                b"HTTP/1.1 200 OK\r\nCache-Control: max-age=0, stale-if-error=60\r\nETag: \"v1\"\r\nContent-Length: 5\r\nConnection: close\r\n\r\nstale",
            )
            .await
            .expect("initial stale response");

        let (mut revalidation, _) = origin.accept().await.expect("304 origin accept");
        let request = read_request_head(&mut revalidation, &mut buffer).await;
        assert!(request.starts_with(b"GET /stale HTTP/1.1\r\n"));
        assert!(
            request
                .windows(19)
                .any(|window| window.eq_ignore_ascii_case(b"if-none-match: \"v1\""))
        );
        revalidation
            .write_all(
                b"HTTP/1.1 304 Not Modified\r\nCache-Control: max-age=0, stale-if-error=60\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .expect("304 response");

        let (mut local, _) = origin.accept().await.expect("local failure origin accept");
        assert_eq!(
            local.read(&mut buffer).await.expect("local failure close"),
            0,
            "locally rejected request reached the origin"
        );

        let (mut failed, _) = origin.accept().await.expect("failed origin accept");
        let request = read_request_head(&mut failed, &mut buffer).await;
        assert!(request.starts_with(b"GET /stale HTTP/1.1\r\n"));
        failed
            .write_all(
                b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .expect("upstream server failure");
    });

    let proxy_address = process_support::reserve_tcp_address();
    let stats_address = process_support::reserve_tcp_address();
    let mut config = forward_cache_config(proxy_address);
    config.stats = Some(Stats {
        binds: vec![stats_address],
        admin_token_file: None,
        pages: Vec::new(),
    });
    let config = config.validate().expect("valid forward proxy config");
    let mut server = process_support::ServerProcess::start(&config, None);
    server.wait_for_tcp(proxy_address).await;
    server.wait_for_tcp(stats_address).await;
    let normal = format!(
        "GET http://{origin_address}/stale HTTP/1.1\r\nHost: origin\r\nConnection: close\r\n\r\n"
    );

    let initial = exchange(proxy_address, normal.as_bytes()).await;
    assert!(initial.starts_with(b"HTTP/1.1 200"));
    assert!(initial.ends_with(b"stale"));
    let revalidated = exchange(proxy_address, normal.as_bytes()).await;
    assert!(revalidated.starts_with(b"HTTP/1.1 200"));
    assert!(revalidated.ends_with(b"stale"));

    let local_failure = exchange_head(
        proxy_address,
        format!(
            "GET http://{origin_address}/stale HTTP/1.1\r\nHost: origin\r\nConnection: bad name\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(local_failure.starts_with(b"HTTP/1.1 400"));

    let stale = exchange(proxy_address, normal.as_bytes()).await;
    assert!(stale.starts_with(b"HTTP/1.1 200"));
    assert!(stale.ends_with(b"stale"));

    let metrics = exchange(
        stats_address,
        b"GET /metrics HTTP/1.1\r\nHost: stats\r\nConnection: close\r\n\r\n",
    )
    .await;
    let metrics = String::from_utf8(metrics).expect("metrics UTF-8");
    assert!(metrics.contains("oxiroute_http_cache_hits_total{listener=\"forward\"} 1"));
    assert!(metrics.contains("oxiroute_http_cache_misses_total{listener=\"forward\"} 4"));
    assert!(metrics.contains("oxiroute_http_cache_admissions_total{listener=\"forward\"} 1"));

    origin_task.await.expect("revalidation origin task");
    server.shutdown();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn cache_cancels_disconnected_leaders_but_not_leaders_with_cancelled_followers() {
    let origin = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let origin_address = origin.local_addr().expect("origin address");
    let origin_task = tokio::spawn(async move {
        let mut buffer = [0; 1024];
        let (mut partial, _) = origin.accept().await.expect("partial origin accept");
        let request = read_request_head(&mut partial, &mut buffer).await;
        assert!(request.starts_with(b"GET /partial HTTP/1.1\r\n"));
        partial
            .write_all(
                b"HTTP/1.1 200 OK\r\nCache-Control: max-age=60\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabc",
            )
            .await
            .expect("partial origin response");
        tokio::time::sleep(Duration::from_millis(150)).await;
        let _ = partial.write_all(b"def").await;

        let (mut retry, _) = origin.accept().await.expect("partial retry accept");
        let request = read_request_head(&mut retry, &mut buffer).await;
        assert!(request.starts_with(b"GET /partial HTTP/1.1\r\n"));
        retry
            .write_all(
                b"HTTP/1.1 200 OK\r\nCache-Control: max-age=60\r\nContent-Length: 5\r\nConnection: close\r\n\r\nfresh",
            )
            .await
            .expect("partial retry response");

        let (mut leader, _) = origin.accept().await.expect("follower leader accept");
        let request = read_request_head(&mut leader, &mut buffer).await;
        assert!(request.starts_with(b"GET /follower HTTP/1.1\r\n"));
        tokio::time::sleep(Duration::from_millis(150)).await;
        leader
            .write_all(
                b"HTTP/1.1 200 OK\r\nCache-Control: max-age=60\r\nContent-Length: 6\r\nConnection: close\r\n\r\nleader",
            )
            .await
            .expect("follower leader response");
    });

    let proxy_address = process_support::reserve_tcp_address();
    let config = forward_cache_config(proxy_address);
    let config = config.validate().expect("valid forward proxy config");
    let mut server = process_support::ServerProcess::start(&config, None);
    server.wait_for_tcp(proxy_address).await;

    let mut disconnected = TcpStream::connect(proxy_address)
        .await
        .expect("partial proxy connection");
    disconnected
        .write_all(
            format!(
                "GET http://{origin_address}/partial HTTP/1.1\r\nHost: origin\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .expect("partial proxy request");
    let mut partial = Vec::new();
    while !partial.ends_with(b"abc") {
        partial.push(
            timeout(Duration::from_secs(30), disconnected.read_u8())
                .await
                .expect("partial response timeout")
                .expect("partial response byte"),
        );
    }
    drop(disconnected);
    tokio::time::sleep(Duration::from_millis(200)).await;

    let retry = exchange(
        proxy_address,
        format!(
            "GET http://{origin_address}/partial HTTP/1.1\r\nHost: origin\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(retry.starts_with(b"HTTP/1.1 200"));
    assert!(retry.ends_with(b"fresh"));

    let follower_request = format!(
        "GET http://{origin_address}/follower HTTP/1.1\r\nHost: origin\r\nConnection: close\r\n\r\n"
    );
    let leader_request = follower_request.clone().into_bytes();
    let leader = tokio::spawn(async move { exchange(proxy_address, &leader_request).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let mut follower = TcpStream::connect(proxy_address)
        .await
        .expect("follower proxy connection");
    follower
        .write_all(follower_request.as_bytes())
        .await
        .expect("follower proxy request");
    tokio::time::sleep(Duration::from_millis(30)).await;
    drop(follower);

    let leader = leader.await.expect("cache leader task");
    assert!(leader.starts_with(b"HTTP/1.1 200"));
    assert!(leader.ends_with(b"leader"));
    tokio::time::sleep(Duration::from_millis(20)).await;
    let cached = exchange(proxy_address, follower_request.as_bytes()).await;
    assert!(cached.starts_with(b"HTTP/1.1 200"));
    assert!(cached.ends_with(b"leader"));

    origin_task.await.expect("cancellation origin task");
    server.shutdown();
}

#[tokio::test]
async fn cache_admits_only_after_eos_and_rejects_trailered_framing() {
    let origin = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let origin_address = origin.local_addr().expect("origin address");
    let origin_task = tokio::spawn(async move {
        let mut buffer = [0; 1024];
        for body in [b"first".as_slice(), b"second".as_slice()] {
            let (mut stream, _) = origin.accept().await.expect("trailered origin accept");
            let request = read_request_head(&mut stream, &mut buffer).await;
            assert!(request.starts_with(b"GET /trailered HTTP/1.1\r\n"));
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nCache-Control: max-age=60\r\nTransfer-Encoding: chunked\r\nTrailer: x-checksum\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("trailered response headers");
            stream
                .write_all(format!("{:x}\r\n", body.len()).as_bytes())
                .await
                .expect("trailered chunk size");
            stream
                .write_all(body)
                .await
                .expect("trailered response body");
            stream
                .write_all(b"\r\n0\r\nx-checksum: present\r\n\r\n")
                .await
                .expect("trailered response end");
        }
    });

    let proxy_address = process_support::reserve_tcp_address();
    let config = forward_cache_config(proxy_address);
    let config = config.validate().expect("valid forward proxy config");
    let mut server = process_support::ServerProcess::start(&config, None);
    server.wait_for_tcp(proxy_address).await;
    let request = format!(
        "GET http://{origin_address}/trailered HTTP/1.1\r\nHost: origin\r\nConnection: close\r\n\r\n"
    );

    let first = exchange(proxy_address, request.as_bytes()).await;
    assert!(first.starts_with(b"HTTP/1.1 200"));
    assert!(first.windows(5).any(|window| window == b"first"));
    let second = exchange(proxy_address, request.as_bytes()).await;
    assert!(second.starts_with(b"HTTP/1.1 200"));
    assert!(second.windows(6).any(|window| window == b"second"));

    origin_task.await.expect("trailered origin task");
    server.shutdown();
}
