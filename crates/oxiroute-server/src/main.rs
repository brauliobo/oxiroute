use std::{
    convert::Infallible,
    error::Error,
    future::Future as _,
    io,
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    pin::Pin,
    process::ExitCode,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    task::{Context, Poll},
    thread::{self, JoinHandle},
    time::{Duration, SystemTime},
};

#[cfg(test)]
use std::sync::atomic::AtomicU64;

use async_trait::async_trait;
use futures_util::FutureExt as _;
use hyper::{server::conn::http1, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use log::{debug, error, info, warn};
use oxiroute_acme::{AcmeOperation, Dns01Cancellation};
use oxiroute_config::ListenerBind;
use oxiroute_rtmp::{
    MAX_PLAYBACK_EVENTS_PER_DRAIN_TURN, RecorderErrorCode, RecorderPhase, RtmpClientSnapshot,
    RtmpControlHandle, RtmpRelayFailure, RtmpServiceHandle, RtmpSessionError, RtmpSessionRole,
    RtmpShutdown,
};
use oxiroute_server::{
    AcmeManagedReconciler, CertbotWatcherConfig, CertbotWatcherSupervisor, ConfigWatcher,
    ConfigWatcherOptions, ConnectionGuard, FileWatcherConfig, FileWatcherSupervisor,
    ForwardConnectionLifecycle, ForwardHttp1ServicePlan, ForwardHttp2ServiceApp, GenerationManager,
    HaproxyStatsApi, HaproxyStatsPage, Http3Runtime, HttpDownstreamPolicyApp, HttpListenerApp,
    HttpReverseProxy, ListenerMetrics, ListenerReservation, ListenerRuntime, MAX_HTTP_ATTEMPTS,
    MonitoredHttpApp, RtmpManagementApi, RtmpServicePlan, RuntimeGeneration, RuntimeMetrics,
    RuntimeReferenceKind, ServiceKind, TcpRelayCore, TlsProfilePlan, TopologySnapshot, UdpRuntime,
    cli::{Cli, Command, ConfigCommand, execute_offline},
    config_coordinator::{CanonicalConfigCoordinator, ConfigLoadOutcome, EffectiveRevision},
    emit_certificate, emit_rtmp_access,
};
use pingora::{
    apps::http_app::HttpServer,
    apps::{AcceptGate, ConnectionAdmission, ServerApp},
    protocols::{ALPN, Stream},
    proxy::http_proxy,
    server::{
        RunArgs, Server, ShutdownSignal, ShutdownSignalWatch, ShutdownWatch,
        configuration::ServerConf,
    },
    services::{
        Service as PingoraService, ServiceReadyNotifier, ServiceWithDependents,
        background::background_service, listening::Service,
    },
};
use signal_hook::{
    consts::signal::{SIGHUP, SIGINT, SIGTERM},
    iterator::Signals,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    time::{Instant, MissedTickBehavior, Sleep, interval, timeout},
};

#[path = "main/forward_apps.rs"]
mod forward_apps;
#[path = "main/generation_process.rs"]
mod generation_process;
#[path = "main/listener_services.rs"]
mod listener_services;
#[path = "main/rtmp_ingest.rs"]
mod rtmp_ingest;
#[cfg(target_os = "linux")]
mod supervised;
#[allow(clippy::wildcard_imports)]
use {forward_apps::*, generation_process::*, listener_services::*, rtmp_ingest::*};
#[cfg(test)]
mod test_ports;
#[cfg(test)]
#[path = "main/tests.rs"]
mod tests;

/// Per-thread allocation caches: with several workers per listener, glibc's shared arenas made every
/// cross-thread free wait on a lock.
#[cfg(target_os = "linux")]
#[global_allocator]
static ALLOCATOR: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

const GENERATION_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

/// Idle upstream connections a worker thread keeps for reuse. Under load every client connection can hold
/// one upstream connection, so a smaller pool closes some on release and opens new ones a moment later.
const UPSTREAM_IDLE_CONNECTIONS: usize = 1024;

#[allow(clippy::too_many_arguments)]
fn build_management_api(
    control: RtmpControlHandle,
    metrics: RuntimeMetrics,
    topology: Arc<TopologySnapshot>,
    coordinator: CanonicalConfigCoordinator,
    active_revision: EffectiveRevision,
    token_file: &Path,
    ui_dir: Option<&Path>,
    mode: oxiroute_server::RuntimeMode,
) -> io::Result<RtmpManagementApi> {
    let api = if let Some(ui_dir) = ui_dir {
        RtmpManagementApi::with_ui_dir(control, metrics, topology, ui_dir)
    } else {
        Ok(RtmpManagementApi::new(control, metrics, topology))
    }?;
    api.with_config_coordinator_from_token_file(coordinator, active_revision, token_file, mode)
}

fn main() -> ExitCode {
    #[cfg(target_os = "linux")]
    if let Some(exit_code) = supervised::dispatch() {
        return exit_code;
    }

    env_logger::init();

    let cli = Cli::parse_process();
    let result = match cli.command() {
        Command::Serve { config } => {
            #[cfg(target_os = "linux")]
            {
                supervised::master::run_if_supported(config)
            }
            #[cfg(not(target_os = "linux"))]
            {
                run(config)
            }
        }
        Command::Import { .. }
        | Command::Registry { .. }
        | Command::Version
        | Command::Config {
            command: ConfigCommand::Check { .. } | ConfigCommand::Compose { .. },
        } => execute_offline(cli.command()).map(|output| {
            if let Some(output) = output {
                print!("{output}");
            }
        }),
        _ => return cli.execute_management(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("OxiRoute startup failed: {error}");
            ExitCode::FAILURE
        }
    }
}

struct ChannelShutdownSignal {
    shutdown: tokio::sync::watch::Receiver<bool>,
}

#[async_trait]
impl ShutdownSignalWatch for ChannelShutdownSignal {
    async fn recv(&self) -> ShutdownSignal {
        let mut shutdown = self.shutdown.clone();
        if !*shutdown.borrow() {
            let _ = shutdown.changed().await;
        }
        ShutdownSignal::GracefulTerminate
    }
}

fn start_config_watcher_or_shutdown<F>(
    coordinator: CanonicalConfigCoordinator,
    manager: &GenerationManager,
    initial: &Arc<Mutex<Option<GenerationProcess>>>,
    start: F,
) -> Result<ConfigWatcher, Box<dyn Error>>
where
    F: FnOnce(
        CanonicalConfigCoordinator,
        GenerationManager,
        ConfigWatcherOptions,
        oxiroute_server::RuntimeMode,
    ) -> notify::Result<ConfigWatcher>,
{
    match start(
        coordinator,
        manager.clone(),
        ConfigWatcherOptions::default(),
        oxiroute_server::RuntimeMode::Direct,
    ) {
        Ok(watcher) => Ok(watcher),
        Err(error) => {
            let process = initial
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(process) = process {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                let _ = shutdown_generation_processes(manager, vec![process], deadline);
            } else {
                error!("initial generation ownership was lost before watcher failure cleanup");
            }
            Err(error.into())
        }
    }
}

#[allow(clippy::too_many_lines)]
fn run(config_path: &Path) -> Result<(), Box<dyn Error>> {
    let stop_supervisor = Arc::new(AtomicBool::new(false));
    let reload_requested = Arc::new(AtomicBool::new(false));
    let mut signals = Signals::new([SIGTERM, SIGINT, SIGHUP])?;
    let signal_handle = signals.handle();
    let signal_stop = Arc::clone(&stop_supervisor);
    let signal_reload = Arc::clone(&reload_requested);
    let signal_thread = thread::Builder::new()
        .name("oxiroute-signals".into())
        .spawn(move || {
            for signal in signals.forever() {
                match signal {
                    SIGTERM | SIGINT => {
                        signal_stop.store(true, Ordering::Release);
                        break;
                    }
                    SIGHUP => signal_reload.store(true, Ordering::Release),
                    _ => {}
                }
            }
        })?;
    let config_coordinator = CanonicalConfigCoordinator::new(config_path)?;
    let config_document = match config_coordinator.load() {
        ConfigLoadOutcome::Loaded(document) => document,
        ConfigLoadOutcome::Rejected(rejection) => {
            return Err(io::Error::new(io::ErrorKind::InvalidData, rejection.message()).into());
        }
    };
    if stop_supervisor.load(Ordering::Acquire) {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "startup was cancelled").into());
    }
    let generation_manager = GenerationManager::new();
    let startup_deadline = std::time::Instant::now() + GENERATION_STARTUP_TIMEOUT;
    let candidate = generation_manager.prepare_with_deadline(*config_document, startup_deadline)?;
    let mut startup = generation_manager.begin_candidate_start(&candidate)?;
    let starting_generation = startup.claim_runtime_start()?;
    let initial = GenerationProcess::start(
        starting_generation,
        config_coordinator.clone(),
        &generation_manager,
        &stop_supervisor,
        startup_deadline,
    )?;
    if let Err(error) =
        wait_for_generation_ready(candidate.generation(), &stop_supervisor, startup_deadline)
    {
        let _ = finish_candidate_generation_process(&generation_manager, initial, startup_deadline);
        signal_handle.close();
        let _ = signal_thread.join();
        return Err(error);
    }
    let supervisor_stop = Arc::clone(&stop_supervisor);
    let supervisor_manager = generation_manager.clone();
    let supervisor_coordinator = config_coordinator.clone();
    let supervisor_healthy = Arc::new(AtomicBool::new(true));
    let thread_supervisor_healthy = Arc::clone(&supervisor_healthy);
    let initial = Arc::new(Mutex::new(Some(initial)));
    let thread_initial = Arc::clone(&initial);
    let (start_supervisor, await_supervisor_start) = mpsc::sync_channel(1);
    let supervisor = match thread::Builder::new()
        .name("oxiroute-generation-supervisor".into())
        .spawn(move || {
            if await_supervisor_start.recv().is_err() {
                return;
            }
            let Some(initial) = thread_initial
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            else {
                thread_supervisor_healthy.store(false, Ordering::Release);
                return;
            };
            if !supervise_generations(
                &supervisor_manager,
                &supervisor_coordinator,
                initial,
                &supervisor_stop,
                &supervisor_stop,
            ) {
                thread_supervisor_healthy.store(false, Ordering::Release);
            }
        }) {
        Ok(supervisor) => supervisor,
        Err(error) => {
            let initial = initial
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(initial) = initial {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                let _ = finish_candidate_generation_process(&generation_manager, initial, deadline);
            }
            signal_handle.close();
            let _ = signal_thread.join();
            return Err(error.into());
        }
    };
    if let Err(error) = startup.activate() {
        drop(start_supervisor);
        let _ = supervisor.join();
        let initial = initial
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(initial) = initial {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let _ = finish_candidate_generation_process(&generation_manager, initial, deadline);
        }
        signal_handle.close();
        let _ = signal_thread.join();
        return Err(error.into());
    }
    let mut config_watcher = match start_config_watcher_or_shutdown(
        config_coordinator,
        &generation_manager,
        &initial,
        ConfigWatcher::start,
    ) {
        Ok(watcher) => watcher,
        Err(error) => {
            drop(start_supervisor);
            let _ = supervisor.join();
            signal_handle.close();
            let _ = signal_thread.join();
            return Err(error);
        }
    };
    if start_supervisor.send(()).is_err() {
        config_watcher.shutdown();
        let initial = initial
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(initial) = initial {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let _ = shutdown_generation_processes(&generation_manager, vec![initial], deadline);
        }
        let _ = supervisor.join();
        signal_handle.close();
        let _ = signal_thread.join();
        return Err(io::Error::other("generation supervisor terminated before startup").into());
    }
    while !stop_supervisor.load(Ordering::Acquire) {
        if reload_requested.swap(false, Ordering::AcqRel) {
            config_watcher.wake();
        }
        thread::sleep(Duration::from_millis(20));
    }
    signal_handle.close();
    let signal_result = signal_thread.join();
    config_watcher.shutdown();
    let supervisor_result = supervisor.join();
    if signal_result.is_err() {
        return Err(io::Error::other("signal thread terminated unexpectedly").into());
    }
    if supervisor_result.is_err() {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        for shutdown in generation_manager.begin_shutdown(deadline) {
            let _ = shutdown.wait_until(deadline);
        }
        return Err(io::Error::other("generation supervisor terminated unexpectedly").into());
    }
    if supervisor_healthy.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err(io::Error::other("active generation runtime terminated unexpectedly").into())
    }
}

#[allow(clippy::too_many_lines)]
fn serve_generation(
    generation: &Arc<RuntimeGeneration>,
    config_coordinator: &CanonicalConfigCoordinator,
    generation_manager: &GenerationManager,
    process_shutdown: Arc<AtomicBool>,
    shutdown: tokio::sync::watch::Receiver<bool>,
    setup: &mpsc::SyncSender<Result<(), String>>,
) -> Result<(), Box<dyn Error>> {
    let active_revision = generation.revision().candidate.clone();
    let config = generation.config().as_draft();
    let management_token_file = if config.management.is_some() {
        Some(PathBuf::from(
            std::env::var_os("OXIROUTE_MANAGEMENT_TOKEN_FILE").ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "authenticated configuration management requires OXIROUTE_MANAGEMENT_TOKEN_FILE",
                )
            })?,
        ))
    } else {
        None
    };

    let server_config = ServerConf {
        max_retries: MAX_HTTP_ATTEMPTS,
        grace_period_seconds: Some(0),
        graceful_shutdown_timeout_seconds: Some(0),
        upstream_keepalive_pool_size: UPSTREAM_IDLE_CONNECTIONS,
        ..ServerConf::default()
    };
    let mut server = Server::new_with_opt_and_conf(None, server_config)?;
    server.bootstrap();
    server.add_service(GenerationRuntimeMarker {
        generation: Arc::clone(generation),
    });

    let workers = listener_workers(config);
    let plan = generation.plan();
    let services = generation.services().to_vec();
    let health_supervisor = generation.health_supervisor();
    let pools = generation.pools().to_vec();
    let tls = generation.tls();
    let challenge_store = tls.challenge_store().clone();
    let topology = Arc::clone(&plan.topology);
    let acme_reconcilers = tls.acme_reconcilers().to_vec();
    let certbot_reconcilers = tls.certbot_reconcilers().to_vec();
    let direct_file_reconcilers = tls.file_reconcilers().to_vec();
    let mut http3_runtimes = Vec::new();
    let mut udp_runtimes = Vec::new();
    if let Some(supervisor) = health_supervisor {
        server.add_service(background_service(
            "upstream health",
            GenerationBackgroundService {
                _generation: Arc::clone(generation),
                inner: supervisor,
            },
        ));
    }
    let runtime_metrics = generation.metrics().clone();
    let mut monitored_services = Vec::with_capacity(services.len());
    for spec in services {
        let metrics = generation.traffic_listener_metrics(&spec.name)?;
        let reservation = generation
            .reservations()
            .get(&spec.name)
            .cloned()
            .expect("candidate reserved every planned listener");
        monitored_services.push((spec, metrics, reservation));
    }
    let services = monitored_services;
    let management_reservation = config
        .management
        .as_ref()
        .map(|_| {
            generation
                .reservations()
                .management()
                .cloned()
                .ok_or_else(|| io::Error::other("management listener was not reserved"))
        })
        .transpose()?;
    let rtmp_control = generation.rtmp_control();
    if let Some(management) = &config.management {
        let metrics = generation.management_listener_metrics()?;
        let management_api = build_management_api(
            rtmp_control.clone(),
            runtime_metrics.clone(),
            Arc::clone(&topology),
            config_coordinator.clone(),
            active_revision.clone(),
            management_token_file
                .as_deref()
                .expect("management token path was required above"),
            management.ui_dir.as_deref(),
            runtime_metrics.supervision_mode(),
        )?
        .with_generation_manager(generation_manager.clone())
        .with_process_shutdown(process_shutdown);
        let app = ProcessAdmissionApp::new(
            HttpListenerApp::new(management_api.into_http_app(), None)
                .with_generation(Arc::clone(generation)),
            metrics.clone(),
            Arc::clone(generation),
        );
        let mut service = Service::new("OxiRoute management".into(), app);
        service.add_tcp(&management.bind.to_string());
        server.add_service(
            RuntimeListenerService::new(
                service,
                management_reservation.expect("management listener reservation was prepared above"),
                Some(metrics),
            )
            .with_generation(Arc::clone(generation)),
        );
        info!("configured management API on {}", management.bind);
    }
    if let Some(stats) = &config.stats {
        for (index, bind) in stats.binds.iter().enumerate() {
            let metrics = generation.stats_listener_metrics(index)?;
            let api = HaproxyStatsApi::new(
                runtime_metrics.clone(),
                pools.clone(),
                rtmp_control.clone(),
                generation_manager.clone(),
                stats.admin_token_file.as_deref(),
            )?;
            let stats_app = ProcessAdmissionApp::new(
                HttpListenerApp::new(HttpServer::new_app(api), None)
                    .with_generation(Arc::clone(generation)),
                metrics.clone(),
                Arc::clone(generation),
            );
            let mut service = Service::new(format!("OxiRoute statistics {index}"), stats_app);
            service.add_tcp(&bind.to_string());
            let reservation = generation
                .reservations()
                .stats(index)
                .cloned()
                .expect("candidate reserved every statistics listener");
            server.add_service(
                RuntimeListenerService::new(service, reservation, Some(metrics))
                    .with_generation(Arc::clone(generation)),
            );
            info!("configured HAProxy-compatible statistics on {bind}");
        }
        for (index, page) in stats.pages.iter().enumerate() {
            let app = HaproxyStatsPage::new(page, generation_manager.clone());
            let metrics = generation.stats_page_listener_metrics(index)?;
            let stats_app = MonitoredHttpApp::new(
                HttpListenerApp::new(
                    HttpDownstreamPolicyApp::new(
                        HttpServer::new_app(app),
                        page.downstream_timeouts,
                    ),
                    None,
                )
                .with_generation(Arc::clone(generation)),
                metrics.clone(),
                Arc::clone(generation),
            );
            let mut service = Service::new(format!("OxiRoute stats page {index}"), stats_app);
            service.add_tcp(&page.bind.to_string());
            let reservation = generation
                .reservations()
                .stats_page(index)
                .cloned()
                .expect("candidate reserved every statistics page listener");
            server.add_service(
                RuntimeListenerService::new(service, reservation, Some(metrics))
                    .with_generation(Arc::clone(generation)),
            );
            info!(
                "configured HAProxy-compatible statistics page on {}",
                page.bind
            );
        }
    }

    for (spec, metrics, reservation) in services {
        let listener_name = spec.name;
        let listener_bind = spec.bind;
        let service_name = format!("OxiRoute {listener_name}");
        let listener_tls = spec.tls;
        let downstream_timeouts = spec.downstream_timeouts;
        let listener_proxy_protocol = spec.proxy_protocol;

        match spec.kind {
            ServiceKind::ForwardHttp1(forward_service) => {
                let mut service = Service::new(
                    service_name,
                    ForwardHttp1RuntimeApp::new(
                        ForwardHttp1App::new(
                            forward_service,
                            metrics.clone(),
                            downstream_timeouts
                                .request_timeout_ms
                                .map(Duration::from_millis),
                            listener_tls.is_some(),
                            challenge_store.clone(),
                        ),
                        metrics.clone(),
                        Arc::clone(generation),
                    ),
                );
                service.threads = Some(workers);
                add_http_listener(
                    &mut service,
                    &listener_name,
                    &listener_bind,
                    listener_tls.as_deref(),
                )?;
                server.add_service(
                    RuntimeListenerService::new(service, reservation, Some(metrics))
                        .with_generation(Arc::clone(generation)),
                );
            }
            ServiceKind::ForwardHttp2(forward_service) => {
                let app = ForwardHttp2App::new(
                    Arc::clone(&forward_service),
                    HttpListenerApp::new(
                        HttpDownstreamPolicyApp::new(
                            ForwardHttp2ServiceApp::new(forward_service),
                            downstream_timeouts,
                        ),
                        listener_tls.as_deref(),
                    )
                    .with_generation(Arc::clone(generation)),
                    metrics.clone(),
                    Arc::clone(generation),
                );
                let mut service = Service::new(service_name, app);
                service.threads = Some(workers);
                add_http_listener(
                    &mut service,
                    &listener_name,
                    &listener_bind,
                    listener_tls.as_deref(),
                )?;
                server.add_service(
                    RuntimeListenerService::new(service, reservation, Some(metrics))
                        .with_generation(Arc::clone(generation)),
                );
            }
            ServiceKind::Http(http_service) => {
                let lane_service = Arc::clone(&http_service);
                let proxy = http_proxy(
                    &server.configuration,
                    HttpReverseProxy::new(http_service, metrics.clone())
                        .with_challenge_store(challenge_store.clone())
                        .with_generation(Arc::clone(generation)),
                );
                let upstream = proxy.upstream_h1_connector();
                let app = MonitoredHttpApp::new(
                    HttpListenerApp::new(
                        HttpDownstreamPolicyApp::new(proxy, downstream_timeouts).with_lane(
                            lane_service,
                            metrics.clone(),
                            Arc::clone(generation),
                            Some(challenge_store.clone()),
                            upstream,
                            config.fast_lane_enabled(),
                        ),
                        listener_tls.as_deref(),
                    )
                    .with_generation(Arc::clone(generation)),
                    metrics.clone(),
                    Arc::clone(generation),
                );
                let mut service = Service::new(service_name, app);
                service.threads = Some(workers);
                add_http_listener(
                    &mut service,
                    &listener_name,
                    &listener_bind,
                    listener_tls.as_deref(),
                )?;
                server.add_service(
                    RuntimeListenerService::new(service, reservation, Some(metrics))
                        .with_generation(Arc::clone(generation)),
                );
            }
            ServiceKind::Rtmp(rtmp_service) => {
                let runtime = generation
                    .rtmp_service(rtmp_service.service_id())
                    .expect("RTMP runtimes were prepared before listener registration");
                let mut service = Service::new(
                    service_name,
                    RtmpIngest::new(
                        listener_name.clone(),
                        runtime,
                        rtmp_control.clone(),
                        rtmp_service,
                        metrics.clone(),
                        Arc::clone(generation),
                    ),
                );
                add_plain_listener(&mut service, &listener_name, &listener_bind)?;
                server.add_service(
                    RuntimeListenerService::new(service, reservation, Some(metrics))
                        .with_generation(Arc::clone(generation)),
                );
            }
            ServiceKind::Tcp(l4_service) => {
                let mut service = Service::new(
                    service_name,
                    TcpRelay::new(
                        l4_service,
                        metrics.clone(),
                        listener_proxy_protocol,
                        Arc::clone(generation),
                    ),
                );
                service.threads = Some(workers);
                add_plain_listener(&mut service, &listener_name, &listener_bind)?;
                server.add_service(
                    RuntimeListenerService::new(service, reservation, Some(metrics))
                        .with_generation(Arc::clone(generation)),
                );
            }
            ServiceKind::ForwardHttp3(forward_service) => {
                let Some(listener_tls) = listener_tls else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("HTTP/3 listener `{listener_name}` requires a TLS profile"),
                    )
                    .into());
                };
                let runtime = Http3Runtime::start_forward(
                    listener_name.clone(),
                    reservation,
                    forward_service,
                    listener_tls,
                    Arc::clone(generation),
                    metrics,
                    shutdown.clone(),
                )
                .inspect_err(|_| {
                    generation.mark_runtime_failed();
                })?;
                http3_runtimes.push(runtime);
            }
            ServiceKind::Http3(http_service) => {
                let Some(listener_tls) = listener_tls else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("HTTP/3 listener `{listener_name}` requires a TLS profile"),
                    )
                    .into());
                };
                let runtime = Http3Runtime::start_reverse(
                    listener_name.clone(),
                    reservation,
                    http_service,
                    listener_tls,
                    Arc::clone(generation),
                    metrics,
                    downstream_timeouts,
                    shutdown.clone(),
                )
                .inspect_err(|_| {
                    generation.mark_runtime_failed();
                })?;
                http3_runtimes.push(runtime);
            }
            ServiceKind::Udp(l4_service) => {
                let runtime = UdpRuntime::start(
                    listener_name.clone(),
                    reservation,
                    l4_service,
                    Arc::clone(generation),
                    metrics,
                    listener_proxy_protocol,
                    shutdown.clone(),
                )
                .inspect_err(|_| {
                    generation.mark_runtime_failed();
                })?;
                udp_runtimes.push(runtime);
            }
        }

        info!("configured {listener_name} on {listener_bind}");
    }

    let mut certbot_watcher = tls.start_certbot_watcher(CertbotWatcherConfig::default())?;
    runtime_metrics.register_acme_managed_monitoring(acme_reconcilers.clone())?;
    runtime_metrics.register_certbot_monitoring(
        certbot_reconcilers,
        certbot_watcher
            .as_ref()
            .map(CertbotWatcherSupervisor::monitor),
    )?;
    let mut direct_file_watcher = tls.start_file_watcher(FileWatcherConfig::default())?;
    runtime_metrics.register_direct_file_monitoring(
        direct_file_reconcilers,
        direct_file_watcher
            .as_ref()
            .map(FileWatcherSupervisor::monitor),
    )?;
    setup
        .send(Ok(()))
        .map_err(|_| io::Error::other("generation setup receiver was dropped"))?;
    let acme_supervisor = AcmeManagedSupervisor::start(acme_reconcilers)?;
    server.run(RunArgs {
        shutdown_signal: Box::new(ChannelShutdownSignal { shutdown }),
    });
    join_generation_runtimes(acme_supervisor, http3_runtimes, udp_runtimes)?;
    if let Some(watcher) = &mut certbot_watcher {
        watcher.shutdown();
    }
    if let Some(watcher) = &mut direct_file_watcher {
        watcher.shutdown();
    }
    Ok(())
}

fn unix_time_ms() -> Option<u64> {
    let duration = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?;
    u64::try_from(duration.as_millis()).ok()
}
