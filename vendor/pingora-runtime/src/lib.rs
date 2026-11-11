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

//! Pingora tokio runtime.
//!
//! Tokio runtime comes in two flavors: a single-threaded runtime
//! and a multi-threaded one which provides work stealing.
//! Benchmark shows that, compared to the single-threaded runtime, the multi-threaded one
//! has some overhead due to its more sophisticated work steal scheduling.
//!
//! This crate provides a third flavor: a multi-threaded runtime without work stealing.
//! This flavor is as efficient as the single-threaded runtime while allows the async
//! program to use multiple cores.

use once_cell::sync::OnceCell;
pub use oxiroute_aged_executor::Age;
use oxiroute_aged_executor::AgedExecutor;
pub use oxiroute_worker_scaling::{
    process_stats as scaling_stats, Policy as ScalingPolicy, Stats as ScalingStats,
};
use oxiroute_worker_scaling::{Scaling, Workers};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::ops::Deref;
#[cfg(feature = "dial9")]
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::runtime::{Builder, Handle};
use tokio::sync::oneshot::{channel, Sender};

/// Default maximum size of a dial9 trace segment file.
#[cfg(feature = "dial9")]
pub const DEFAULT_DIAL9_MAX_FILE_SIZE: u64 = 100 * 1024 * 1024;
/// Default maximum bytes retained locally by dial9.
#[cfg(feature = "dial9")]
pub const DEFAULT_DIAL9_MAX_TOTAL_SIZE: u64 = 512 * 1024 * 1024;

/// Configuration options for the blocking thread pool used by the runtime.
///
/// These options control the behavior of the blocking thread pool that handles
/// [`tokio::task::spawn_blocking`] tasks.
#[derive(Debug, Clone, Default)]
pub struct BlockingPoolOpts {
    /// The maximum number of threads in the blocking thread pool.
    ///
    /// When not set, the tokio default (512) is used.
    pub max_threads: Option<usize>,
    /// The duration that idle blocking threads are kept alive before being shut down.
    ///
    /// When not set, the tokio default (10 seconds) is used.
    pub thread_keep_alive: Option<Duration>,
}

/// Configuration options for runtime metrics collection.
#[derive(Debug, Clone, Default)]
pub struct RuntimeMetricsOpts {
    /// Enable Tokio's poll-time histogram on the runtime.
    ///
    /// This must be configured before the runtime is built. Enabling it adds
    /// two timestamp reads to every task poll.
    pub poll_time_histogram: bool,
    /// Histogram bucket scale for Tokio's poll-time histogram.
    pub poll_time_histogram_scale: Option<RuntimeMetricsPollTimeHistogramScale>,
    /// Width of the first histogram bucket.
    pub poll_time_histogram_resolution: Option<Duration>,
    /// Number of histogram buckets. Memory usage scales with runtimes × workers × buckets.
    pub poll_time_histogram_buckets: Option<usize>,
}

/// Configuration options for a Tokio runtime.
#[derive(Debug, Clone, Default)]
pub struct RuntimeOpts {
    /// Options for runtime metrics collection.
    pub metrics: RuntimeMetricsOpts,
    /// Enable Tokio's experimental alternative timer.
    ///
    /// This requires building with `--cfg tokio_unstable` and only applies to
    /// Tokio's multi-threaded runtime.
    pub enable_alt_timer: bool,
    /// OxiRoute: on a runtime without work stealing, the connection tasks started with [`spawn_connection`] run
    /// newest first and the oldest first once it has waited this long. `None` keeps Tokio's first-in first-out queue.
    pub connection_age: Option<Age>,
    /// OxiRoute: with `connection_age` set, keep only as many workers active as the load needs: new connections go
    /// to the active ones, and idle connections move off released or crowded workers ([`relocate_connection`]).
    pub worker_scaling: Option<ScalingPolicy>,
    /// Options for dial9 Tokio telemetry.
    #[cfg(feature = "dial9")]
    pub dial9: Option<Dial9RuntimeOpts>,
}

/// Configuration options for dial9 Tokio telemetry.
#[cfg(feature = "dial9")]
#[derive(Debug, Clone)]
pub struct Dial9RuntimeOpts {
    /// Trace output path after server configuration defaults are applied.
    pub trace_path: PathBuf,
    /// Rotate trace segments after this many bytes.
    pub max_file_size: u64,
    /// Maximum bytes retained on local disk.
    pub max_total_size: u64,
    /// Wall-clock trace rotation period.
    pub rotation_period: Option<Duration>,
    /// Enable dial9 task spawn/terminate tracking.
    pub task_tracking: bool,
    /// How often the background worker checks for sealed trace segments.
    pub worker_poll_interval: Option<Duration>,
    /// Upload sealed trace segments to S3-compatible storage.
    #[cfg(feature = "dial9-worker-s3")]
    pub s3_upload: Option<Dial9S3UploadOpts>,
}

#[cfg(feature = "dial9")]
impl Dial9RuntimeOpts {
    /// Create dial9 runtime options using Pingora's dial9 defaults.
    pub fn new(trace_path: impl Into<PathBuf>) -> Self {
        Self {
            trace_path: trace_path.into(),
            max_file_size: DEFAULT_DIAL9_MAX_FILE_SIZE,
            max_total_size: DEFAULT_DIAL9_MAX_TOTAL_SIZE,
            rotation_period: None,
            task_tracking: true,
            worker_poll_interval: None,
            #[cfg(feature = "dial9-worker-s3")]
            s3_upload: None,
        }
    }

    /// Set the maximum size of each trace segment file.
    pub fn with_max_file_size(mut self, max_file_size: u64) -> Self {
        self.max_file_size = max_file_size;
        self
    }

    /// Set the maximum bytes retained on local disk.
    pub fn with_max_total_size(mut self, max_total_size: u64) -> Self {
        self.max_total_size = max_total_size;
        self
    }

    /// Set the wall-clock trace rotation period.
    pub fn with_rotation_period(mut self, rotation_period: Duration) -> Self {
        self.rotation_period = Some(rotation_period);
        self
    }

    /// Enable or disable dial9 task spawn/terminate tracking.
    pub fn with_task_tracking(mut self, task_tracking: bool) -> Self {
        self.task_tracking = task_tracking;
        self
    }

    /// Set how often the background worker checks for sealed trace segments.
    pub fn with_worker_poll_interval(mut self, worker_poll_interval: Duration) -> Self {
        self.worker_poll_interval = Some(worker_poll_interval);
        self
    }

    /// Set S3-compatible upload options for sealed trace segments.
    #[cfg(feature = "dial9-worker-s3")]
    pub fn with_s3_upload(mut self, s3_upload: Dial9S3UploadOpts) -> Self {
        self.s3_upload = Some(s3_upload);
        self
    }
}

/// Configuration options for dial9 S3-compatible trace uploads.
#[cfg(all(feature = "dial9", feature = "dial9-worker-s3"))]
#[derive(Debug, Clone)]
pub struct Dial9S3UploadOpts {
    /// S3 bucket that receives sealed trace segments.
    pub bucket: String,
    /// Service name included in uploaded object keys.
    pub service_name: String,
    /// Optional key prefix.
    pub prefix: Option<String>,
    /// Optional region override.
    pub region: Option<String>,
    /// Optional instance identifier included in uploaded object keys.
    pub instance_path: Option<String>,
    /// Optional pre-built S3 client for custom credentials or endpoints.
    pub client: Option<aws_sdk_s3::Client>,
}

#[cfg(all(feature = "dial9", feature = "dial9-worker-s3"))]
impl Dial9S3UploadOpts {
    /// Create S3-compatible upload options.
    pub fn new(bucket: impl Into<String>, service_name: impl Into<String>) -> Self {
        Self {
            bucket: bucket.into(),
            service_name: service_name.into(),
            prefix: None,
            region: None,
            instance_path: None,
            client: None,
        }
    }

    /// Set the object key prefix.
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = Some(prefix.into());
        self
    }

    /// Set the AWS region override.
    pub fn with_region(mut self, region: impl Into<String>) -> Self {
        self.region = Some(region.into());
        self
    }

    /// Set the instance identifier included in uploaded object keys.
    pub fn with_instance_path(mut self, instance_path: impl Into<String>) -> Self {
        self.instance_path = Some(instance_path.into());
        self
    }

    /// Set a pre-built S3 client for custom credentials or endpoints.
    pub fn with_client(mut self, client: aws_sdk_s3::Client) -> Self {
        self.client = Some(client);
        self
    }
}

/// Bucket scale for Tokio's poll-time histogram.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMetricsPollTimeHistogramScale {
    /// Equal-width buckets.
    Linear,
    /// Buckets double in width at each step.
    Log,
}

/// Pingora async multi-threaded runtime
///
/// The `Steal` flavor is effectively tokio multi-threaded runtime.
///
/// The `NoSteal` flavor is backed by multiple tokio single-threaded runtime.
pub enum Runtime {
    Steal {
        runtime: tokio::runtime::Runtime,
        #[cfg(feature = "dial9")]
        dial9_guard: Option<dial9_tokio_telemetry::telemetry::TelemetryGuard>,
    },
    NoSteal(NoStealRuntime),
}

/// Apply [`BlockingPoolOpts`] to a tokio [`Builder`].
fn apply_blocking_opts(builder: &mut Builder, opts: &BlockingPoolOpts) {
    if let Some(max) = opts.max_threads {
        builder.max_blocking_threads(max);
    }
    if let Some(ttl) = opts.thread_keep_alive {
        builder.thread_keep_alive(ttl);
    }
}

/// Apply [`RuntimeMetricsOpts`] to a tokio [`Builder`].
// The replacement `metrics_poll_time_histogram_configuration` API is not used
// here so this crate can continue to compile against older Tokio 1.x versions
// selected by downstream applications while still honoring these knobs in
// tokio-unstable builds.
#[allow(deprecated)]
fn apply_metrics_opts(builder: &mut Builder, opts: &RuntimeMetricsOpts) {
    #[cfg(tokio_unstable)]
    if opts.poll_time_histogram {
        builder.enable_metrics_poll_time_histogram();

        if let Some(scale) = opts.poll_time_histogram_scale {
            builder.metrics_poll_count_histogram_scale(match scale {
                RuntimeMetricsPollTimeHistogramScale::Linear => {
                    tokio::runtime::HistogramScale::Linear
                }
                RuntimeMetricsPollTimeHistogramScale::Log => tokio::runtime::HistogramScale::Log,
            });
        }
        if let Some(resolution) = opts
            .poll_time_histogram_resolution
            .filter(|resolution| !resolution.is_zero())
        {
            builder.metrics_poll_count_histogram_resolution(resolution);
        }
        if let Some(buckets) = opts
            .poll_time_histogram_buckets
            .filter(|buckets| *buckets > 0)
        {
            builder.metrics_poll_count_histogram_buckets(buckets);
        }
    }

    #[cfg(not(tokio_unstable))]
    let _ = (builder, opts);
}

/// Apply timer options from [`RuntimeOpts`] to a tokio [`Builder`].
fn apply_timer_opts(builder: &mut Builder, opts: &RuntimeOpts) {
    #[cfg(tokio_unstable)]
    if opts.enable_alt_timer {
        builder.enable_alt_timer();
    }

    #[cfg(not(tokio_unstable))]
    let _ = (builder, opts);
}

#[cfg(feature = "dial9")]
fn build_dial9_runtime(
    builder: Builder,
    runtime_name: &str,
    opts: &Dial9RuntimeOpts,
) -> std::io::Result<(
    tokio::runtime::Runtime,
    dial9_tokio_telemetry::telemetry::TelemetryGuard,
)> {
    use dial9_tokio_telemetry::telemetry::{RotatingWriter, TracedRuntime};
    use std::io::{Error, ErrorKind};

    if opts.max_file_size == 0 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "dial9 max_file_size must be greater than zero",
        ));
    }
    if opts.max_total_size == 0 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "dial9 max_total_size must be greater than zero",
        ));
    }
    if opts.max_file_size > opts.max_total_size {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "dial9 max_file_size must be less than or equal to max_total_size",
        ));
    }
    if opts.worker_poll_interval == Some(Duration::ZERO) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "dial9 worker_poll_interval must be greater than zero",
        ));
    }

    if let Some(parent) = opts.trace_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let writer = RotatingWriter::builder()
        .base_path(opts.trace_path.clone())
        .max_file_size(opts.max_file_size)
        .max_total_size(opts.max_total_size)
        .maybe_rotation_period(opts.rotation_period)
        .build()?;

    let mut traced = TracedRuntime::builder()
        .with_trace_path(opts.trace_path.clone())
        .with_runtime_name(runtime_name)
        .with_task_tracking(opts.task_tracking);
    if let Some(worker_poll_interval) = opts.worker_poll_interval {
        traced = traced.with_worker_poll_interval(worker_poll_interval);
    }

    #[cfg(feature = "dial9-worker-s3")]
    if let Some(s3_upload) = &opts.s3_upload {
        if s3_upload.bucket.trim().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "dial9 s3 bucket must not be empty",
            ));
        }
        if s3_upload.service_name.trim().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "dial9 s3 service_name must not be empty",
            ));
        }
        let s3_config = dial9_tokio_telemetry::background_task::s3::S3Config::builder()
            .bucket(s3_upload.bucket.clone())
            .service_name(s3_upload.service_name.clone())
            .maybe_prefix(s3_upload.prefix.clone())
            .maybe_region(s3_upload.region.clone())
            .maybe_instance_path(s3_upload.instance_path.clone());
        let traced = traced.with_s3_uploader(s3_config.build());
        if let Some(client) = s3_upload.client.clone() {
            return traced
                .with_s3_client(client)
                .build_and_start(builder, writer);
        }
        return traced.build_and_start(builder, writer);
    }

    traced.build_and_start(builder, writer)
}

/// Builder for constructing a [`Runtime`].
///
/// # Example
///
/// ```
/// use pingora_runtime::{RuntimeBuilder, BlockingPoolOpts};
/// use std::time::Duration;
///
/// let rt = RuntimeBuilder::new(4, "my-service")
///     .blocking_pool_opts(BlockingPoolOpts {
///         max_threads: Some(64),
///         thread_keep_alive: Some(Duration::from_secs(30)),
///     })
///     .build();
/// ```
pub struct RuntimeBuilder {
    threads: usize,
    name: String,
    work_steal: bool,
    blocking_pool_opts: BlockingPoolOpts,
    runtime_opts: RuntimeOpts,
}

impl RuntimeBuilder {
    /// Create a new builder with the given number of worker threads and runtime name.
    ///
    /// Work stealing is enabled by default.
    pub fn new(threads: usize, name: &str) -> Self {
        Self {
            threads,
            name: name.to_string(),
            work_steal: true,
            blocking_pool_opts: BlockingPoolOpts::default(),
            runtime_opts: RuntimeOpts::default(),
        }
    }

    /// Set whether work stealing is enabled.
    ///
    /// When `true` (the default), a tokio multi-thread runtime is used.
    /// When `false`, a pool of single-threaded tokio runtimes is used instead.
    pub fn work_steal(mut self, enabled: bool) -> Self {
        self.work_steal = enabled;
        self
    }

    /// Set the [`BlockingPoolOpts`] for the runtime's blocking thread pool.
    pub fn blocking_pool_opts(mut self, opts: BlockingPoolOpts) -> Self {
        self.blocking_pool_opts = opts;
        self
    }

    /// Set the [`RuntimeMetricsOpts`] for the runtime.
    pub fn metrics_opts(mut self, opts: RuntimeMetricsOpts) -> Self {
        self.runtime_opts.metrics = opts;
        self
    }

    /// Set the [`RuntimeOpts`] for the runtime.
    pub fn runtime_opts(mut self, opts: RuntimeOpts) -> Self {
        self.runtime_opts = opts;
        self
    }

    /// Set whether Tokio's experimental alternative timer is enabled.
    ///
    /// This requires building with `--cfg tokio_unstable` and only applies to
    /// work-stealing runtimes.
    pub fn enable_alt_timer(mut self, enabled: bool) -> Self {
        self.runtime_opts.enable_alt_timer = enabled;
        self
    }

    fn build_work_stealing_tokio_builder(&self) -> Builder {
        let mut builder = Builder::new_multi_thread();
        builder
            .enable_all()
            .worker_threads(self.threads)
            .thread_name(&self.name);
        apply_blocking_opts(&mut builder, &self.blocking_pool_opts);
        apply_metrics_opts(&mut builder, &self.runtime_opts.metrics);
        apply_timer_opts(&mut builder, &self.runtime_opts);
        builder
    }

    /// Build the [`Runtime`].
    pub fn build(self) -> Runtime {
        if self.work_steal {
            let mut builder = self.build_work_stealing_tokio_builder();
            #[cfg(feature = "dial9")]
            let dial9_guard = if let Some(dial9_opts) = &self.runtime_opts.dial9 {
                let runtime_name = self.name.clone();
                match build_dial9_runtime(builder, &runtime_name, dial9_opts) {
                    Ok((runtime, guard)) => {
                        return Runtime::Steal {
                            runtime,
                            dial9_guard: Some(guard),
                        };
                    }
                    Err(e) => {
                        log::warn!(
                            "failed to initialize dial9 runtime telemetry for {runtime_name}: {e}"
                        );
                        builder = self.build_work_stealing_tokio_builder();
                        None
                    }
                }
            } else {
                None
            };
            let runtime = builder
                .build()
                .expect("failed to build work-stealing Tokio runtime");
            Runtime::Steal {
                runtime,
                #[cfg(feature = "dial9")]
                dial9_guard,
            }
        } else {
            #[cfg(feature = "dial9")]
            if self.runtime_opts.dial9.is_some() {
                log::warn!("dial9 runtime telemetry is ignored when work stealing is disabled");
            }
            Runtime::NoSteal(NoStealRuntime::new(
                self.threads,
                &self.name,
                self.blocking_pool_opts,
                self.runtime_opts,
            ))
        }
    }
}

impl Runtime {
    /// Create a `Steal` flavor runtime. This just a regular tokio runtime
    pub fn new_steal(threads: usize, name: &str) -> Self {
        RuntimeBuilder::new(threads, name).build()
    }

    /// Create a `NoSteal` flavor runtime. This is backed by multiple tokio current-thread runtime
    pub fn new_no_steal(threads: usize, name: &str) -> Self {
        RuntimeBuilder::new(threads, name).work_steal(false).build()
    }

    /// Return the &[Handle] of the [Runtime].
    /// For `Steal` flavor, it will just return the &[Handle].
    /// For `NoSteal` flavor, it will return the &[Handle] of a random thread in its pool.
    /// So if we want tasks to spawn on all the threads, call this function to get a fresh [Handle]
    /// for each async task.
    pub fn get_handle(&self) -> &Handle {
        match self {
            Self::Steal { runtime, .. } => runtime.handle(),
            Self::NoSteal(r) => r.get_runtime(),
        }
    }

    /// Call tokio's `shutdown_timeout` of all the runtimes. This function is blocking until
    /// all runtimes exit.
    pub fn shutdown_timeout(self, timeout: Duration) {
        match self {
            Self::Steal {
                runtime,
                #[cfg(feature = "dial9")]
                dial9_guard,
            } => {
                #[cfg(feature = "dial9")]
                drop(dial9_guard);
                runtime.shutdown_timeout(timeout);
            }
            Self::NoSteal(r) => r.shutdown_timeout(timeout),
        }
    }
}

struct Pool {
    handle: Handle,
    /// Connections placed on this worker and not yet finished.
    load: Arc<AtomicUsize>,
    /// Runs the connection tasks of the worker in aged newest-first order instead of Tokio's first-in first-out.
    executor: Option<AgedExecutor>,
}

/// The workers of a non-stealing runtime and, when it scales them, which are active.
struct Table {
    pools: Box<[Pool]>,
    scaling: Option<Arc<Scaling>>,
}

impl Deref for Table {
    type Target = [Pool];

    fn deref(&self) -> &[Pool] {
        &self.pools
    }
}

/// What the scaling controller reads of the workers; it holds no reference to the table it controls.
struct PoolWorkers(Vec<(AgedExecutor, Arc<AtomicUsize>)>);

impl Workers for PoolWorkers {
    fn len(&self) -> usize {
        self.0.len()
    }

    fn busy_nanos(&self, worker: usize) -> u64 {
        self.0[worker].0.busy_nanos()
    }

    fn connections(&self, worker: usize) -> usize {
        self.0[worker].1.load(Ordering::Relaxed)
    }
}

struct Local {
    pools: Pools,
    index: usize,
}

// A std thread local: the `thread_local` crate keeps an entry per thread identifier and hands it to any later thread
// that reuses the identifier, which would give a worker of a new runtime the worker table of a shut down one.
// Only NoStealRuntime workers set it.
thread_local! {
    static CURRENT_HANDLE: std::cell::OnceCell<Local> = const { std::cell::OnceCell::new() };
}

/// The worker table and index of the calling thread when it is a worker of a non-stealing runtime.
fn current_worker() -> Option<(Pools, usize)> {
    CURRENT_HANDLE.with(|local| local.get().map(|local| (local.pools.clone(), local.index)))
}

/// Runs `f` with the worker table and index of the calling thread, without taking a reference count (the count is
/// shared by every worker).
fn with_worker<T>(f: impl FnOnce(&Table, usize) -> T) -> Option<T> {
    CURRENT_HANDLE.with(|local| {
        let local = local.get()?;
        Some(f(local.pools.get()?, local.index))
    })
}

/// Return the [Handle] of current runtime.
/// If the current thread is under a `Steal` runtime, the current [Handle] is returned.
/// If the current thread is under a `NoSteal` runtime, the [Handle] of a random thread
/// under this runtime is returned. This function will panic if called outside any runtime.
pub fn current_handle() -> Handle {
    if let Some((pools, _)) = current_worker() {
        // safety: the table is set when the pool is being initialized in init_pools()
        let pools = pools.get().unwrap();
        let mut rng = rand::thread_rng();
        let index = rng.gen_range(0..pools.len());
        pools[index].handle.clone()
    } else {
        // not NoStealRuntime, just check the current tokio runtime
        Handle::current()
    }
}

/// Spawns a task of a connection that is already running, such as an HTTP/2 stream. Under aged queues it runs on the
/// calling worker's executor, so the connection's data stays on one core and the task is served in the same order;
/// otherwise it is spawned like any other task, on a random worker of a non-stealing runtime.
pub fn spawn_stream<F>(future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    match current_worker() {
        Some((pools, own)) => match &pools.get().unwrap()[own].executor {
            Some(executor) => executor.spawn(future),
            None => {
                current_handle().spawn(future);
            }
        },
        None => {
            Handle::current().spawn(future);
        }
    }
}

struct LoadGuard {
    pools: Pools,
    /// The worker the connection is counted on: it changes when the connection moves.
    index: AtomicUsize,
}

tokio::task_local! {
    /// The guard of the connection being polled, for a runtime that scales its workers.
    static CONNECTION: LoadGuard;
}

impl Drop for LoadGuard {
    fn drop(&mut self) {
        self.pools.get().unwrap()[self.index.load(Ordering::Relaxed)]
            .load
            .fetch_sub(1, Ordering::Relaxed);
    }
}

/// Spawns a connection task. On a non-stealing runtime the least loaded worker takes it (ties go to the
/// calling worker), among the active ones when the runtime scales its workers; on a work stealing runtime it is a
/// plain `spawn`.
pub fn spawn_connection<F>(future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    spawn_connection_as(true, future);
}

/// [`spawn_connection`] for a connection that `movable` says can later move to another worker. One that cannot (TLS,
/// HTTP/2) is spread over every worker, active or not: packing it onto the active ones would pin it there when the load
/// rises, as it cannot follow the activation of more workers.
pub fn spawn_connection_as<F>(movable: bool, future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    let Some((worker_pools, own)) = current_worker() else {
        tokio::spawn(future);
        return;
    };
    let pools = worker_pools.get().unwrap();
    let index = match &pools.scaling {
        Some(scaling) if movable => scaling.place(own, |index| pools[index].load.load(Ordering::Relaxed)),
        _ => least_loaded(pools, own),
    };
    pools[index].load.fetch_add(1, Ordering::Relaxed);
    let guard = LoadGuard {
        pools: worker_pools.clone(),
        index: AtomicUsize::new(index),
    };
    match (&pools[index].executor, &pools.scaling) {
        (Some(executor), Some(scaling)) => {
            executor.spawn(CONNECTION.scope(guard, future));
            scaling.connection_placed();
        }
        (executor, _) => {
            let task = async move {
                let _guard = guard;
                future.await
            };
            match executor {
                Some(executor) => executor.spawn(task),
                None => {
                    pools[index].handle.spawn(task);
                }
            }
        }
    }
}

fn least_loaded(pools: &[Pool], own: usize) -> usize {
    let count = pools.len();
    (0..count)
        .map(|step| (own + step) % count)
        .min_by_key(|index| pools[*index].load.load(Ordering::Relaxed))
        .unwrap()
}

/// Whether the calling connection should move to another worker: it sits on a released one, or on one holding more
/// connections than its share. A few loads and no reference count, so a connection can ask between two requests.
pub fn wants_relocation() -> bool {
    with_worker(|pools, own| {
        let load = pools[own].load.load(Ordering::Relaxed);
        pools
            .scaling
            .as_ref()
            .is_some_and(|scaling| scaling.wants_relocation(own, load))
    })
    .unwrap_or(false)
}

/// Moves the calling connection task, which must be between two requests with nothing parked, to a lighter active
/// worker; true when it now runs there. The caller has to move what is tied to the old worker's thread (its socket's
/// I/O registration) before it waits for anything.
pub async fn relocate_connection() -> bool {
    if !oxiroute_aged_executor::in_task() {
        return false;
    }
    let target = with_worker(|pools, own| {
        let scaling = pools.scaling.as_ref()?;
        let to = scaling.relocation(own, |index| pools[index].load.load(Ordering::Relaxed))?;
        let executor = pools[to].executor.clone()?;
        pools[own].load.fetch_sub(1, Ordering::Relaxed);
        pools[to].load.fetch_add(1, Ordering::Relaxed);
        scaling.note_relocated();
        Some((to, executor))
    })
    .flatten();
    let Some((to, executor)) = target else {
        return false;
    };
    CONNECTION.with(|guard| guard.index.store(to, Ordering::Relaxed));
    oxiroute_aged_executor::move_to(executor).await
}

type Control = (Sender<Duration>, JoinHandle<()>);
type Pools = Arc<OnceCell<Table>>;

/// Multi-threaded runtime backed by a pool of single threaded tokio runtime
pub struct NoStealRuntime {
    threads: usize,
    name: String,
    blocking_opts: BlockingPoolOpts,
    runtime_opts: RuntimeOpts,
    // Lazily init the runtimes so that they are created after pingora
    // daemonize itself. Otherwise the runtime threads are lost.
    pools: Pools,
    controls: OnceCell<Vec<Control>>,
}

impl NoStealRuntime {
    /// Create a new [`NoStealRuntime`] with blocking pool options. Panic if `threads` is 0.
    pub fn new(
        threads: usize,
        name: &str,
        blocking_opts: BlockingPoolOpts,
        runtime_opts: RuntimeOpts,
    ) -> Self {
        assert!(threads != 0);
        NoStealRuntime {
            threads,
            name: name.to_string(),
            blocking_opts,
            runtime_opts,
            pools: Arc::new(OnceCell::new()),
            controls: OnceCell::new(),
        }
    }

    fn init_pools(&self) -> (Table, Vec<Control>) {
        let mut pools = Vec::with_capacity(self.threads);
        let mut controls = Vec::with_capacity(self.threads);
        for index in 0..self.threads {
            let mut builder = Builder::new_current_thread();
            builder.enable_all();
            if self.runtime_opts.connection_age.is_some() {
                // The aged executor polls a batch of connections per scheduler tick; poll the I/O driver after each.
                builder.event_interval(1);
            }
            apply_blocking_opts(&mut builder, &self.blocking_opts);
            apply_metrics_opts(&mut builder, &self.runtime_opts.metrics);
            let rt = builder
                .build()
                .expect("failed to build no-steal Tokio runtime worker");
            let handler = rt.handle().clone();
            let scaled = self.runtime_opts.worker_scaling.is_some();
            let executor = self.runtime_opts.connection_age.map(|max_age| {
                let (executor, runner) = if scaled {
                    AgedExecutor::new_measured(max_age)
                } else {
                    AgedExecutor::new(max_age)
                };
                rt.spawn(runner);
                executor
            });
            let (tx, rx) = channel::<Duration>();
            let pools_ref = self.pools.clone();
            let join = std::thread::Builder::new()
                .name(self.name.clone())
                .spawn(move || {
                    CURRENT_HANDLE.with(|local| {
                        let _ = local.set(Local {
                            pools: pools_ref,
                            index,
                        });
                    });
                    if let Ok(timeout) = rt.block_on(rx) {
                        rt.shutdown_timeout(timeout);
                    } // else Err(_): tx is dropped, just exit
                })
                .unwrap();
            pools.push(Pool {
                handle: handler,
                load: Arc::new(AtomicUsize::new(0)),
                executor,
            });
            controls.push((tx, join));
        }

        (self.scaled(pools), controls)
    }

    /// The table of `pools`, with the scaling controller running when the options ask for one and every worker has an
    /// executor to measure.
    fn scaled(&self, pools: Vec<Pool>) -> Table {
        let workers = pools
            .iter()
            .map(|pool| Some((pool.executor.clone()?, Arc::clone(&pool.load))))
            .collect::<Option<Vec<_>>>();
        let policy = self
            .runtime_opts
            .worker_scaling
            .filter(|policy| pools.len() > policy.min_active);
        let scaling = policy.zip(workers).map(|(policy, workers)| {
            let scaling = Arc::new(Scaling::new(&self.name, policy, pools.len()));
            scaling.start(Arc::new(PoolWorkers(workers)));
            scaling
        });
        Table {
            pools: pools.into_boxed_slice(),
            scaling,
        }
    }

    /// What the scaling controller of this runtime has decided, if it runs one.
    pub fn scaling_stats(&self) -> Option<ScalingStats> {
        self.pools
            .get()?
            .scaling
            .as_ref()
            .map(|scaling| scaling.stats())
    }

    /// Return the &[Handle] of a random thread of this runtime
    pub fn get_runtime(&self) -> &Handle {
        let mut rng = rand::thread_rng();

        let index = rng.gen_range(0..self.threads);
        self.get_runtime_at(index)
    }

    /// Return the number of threads of this runtime
    pub fn threads(&self) -> usize {
        self.threads
    }

    fn get_pools(&self) -> &Table {
        if let Some(p) = self.pools.get() {
            p
        } else {
            // TODO: use a mutex to avoid creating a lot threads only to drop them
            let (pools, controls) = self.init_pools();
            // there could be another thread racing with this one to init the pools
            match self.pools.try_insert(pools) {
                Ok(p) => {
                    // unwrap to make sure that this is the one that init both pools and controls
                    self.controls.set(controls).unwrap();
                    p
                }
                // another thread already set it, just return it
                Err((p, _my_pools)) => p,
            }
        }
    }

    /// Return the &[Handle] of a given thread of this runtime
    pub fn get_runtime_at(&self, index: usize) -> &Handle {
        &self.get_pools()[index].handle
    }

    /// Call tokio's `shutdown_timeout` of all the runtimes. This function is blocking until
    /// all runtimes exit.
    pub fn shutdown_timeout(mut self, timeout: Duration) {
        if let Some(scaling) = self.pools.get().and_then(|table| table.scaling.as_ref()) {
            scaling.stop();
        }
        if let Some(controls) = self.controls.take() {
            let (txs, joins): (Vec<Sender<_>>, Vec<JoinHandle<()>>) = controls.into_iter().unzip();
            for tx in txs {
                let _ = tx.send(timeout); // Err() when rx is dropped
            }
            for join in joins {
                let _ = join.join(); // ignore thread error
            }
        } // else, the controls and the runtimes are not even init yet, just return;
    }

    // TODO: runtime metrics
}

#[test]
fn test_steal_runtime() {
    use tokio::time::{sleep, Duration};
    let threads = 2;
    let rt = Runtime::new_steal(threads, "test");
    let handle = rt.get_handle();
    let ret = handle.block_on(async {
        sleep(Duration::from_secs(1)).await;
        let handle = current_handle();
        let join = handle.spawn(async {
            sleep(Duration::from_secs(1)).await;
        });
        join.await.unwrap();
        1
    });

    #[cfg(target_os = "linux")]
    assert_eq!(handle.metrics().num_workers(), threads);
    assert_eq!(ret, 1);
}

#[test]
fn test_no_steal_runtime() {
    use tokio::time::{sleep, Duration};

    let rt = Runtime::new_no_steal(2, "test");
    let handle = rt.get_handle();
    let ret = handle.block_on(async {
        sleep(Duration::from_secs(1)).await;
        let handle = current_handle();
        let join = handle.spawn(async {
            sleep(Duration::from_secs(1)).await;
        });
        join.await.unwrap();
        1
    });

    assert_eq!(ret, 1);
}

#[test]
fn test_no_steal_shutdown() {
    use tokio::time::{sleep, Duration};

    let rt = Runtime::new_no_steal(2, "test");
    let handle = rt.get_handle();
    let ret = handle.block_on(async {
        sleep(Duration::from_secs(1)).await;
        let handle = current_handle();
        let join = handle.spawn(async {
            sleep(Duration::from_secs(1)).await;
        });
        join.await.unwrap();
        1
    });
    assert_eq!(ret, 1);

    rt.shutdown_timeout(Duration::from_secs(1));
}

#[cfg(feature = "dial9")]
#[test]
fn test_dial9_zero_worker_poll_interval_is_rejected() {
    let mut opts = Dial9RuntimeOpts::new("trace");
    opts.worker_poll_interval = Some(Duration::ZERO);
    let err = match build_dial9_runtime(Builder::new_multi_thread(), "test", &opts) {
        Ok(_) => panic!("zero worker poll interval should be rejected"),
        Err(err) => err,
    };

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(
        err.to_string(),
        "dial9 worker_poll_interval must be greater than zero"
    );
}

#[cfg(test)]
mod scaling_tests;

#[cfg(test)]
mod connection_tests {
    use super::*;
    use std::collections::HashMap;
    use std::thread::ThreadId;
    use tokio::sync::{mpsc, Notify};
    use tokio::time::{sleep, Duration};

    fn aged_runtime(threads: usize, name: &str, age: Duration) -> Runtime {
        RuntimeBuilder::new(threads, name)
            .work_steal(false)
            .runtime_opts(RuntimeOpts {
                connection_age: Some(Age::Fixed(age)),
                ..RuntimeOpts::default()
            })
            .build()
    }

    /// Spawns `count` connection tasks from a worker of `runtime`; each reports its thread and then waits to be
    /// released. Returns the number of tasks per worker thread.
    fn spread(runtime: &Runtime, count: usize) -> (HashMap<ThreadId, usize>, Arc<Notify>) {
        let release = Arc::new(Notify::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let release_all = release.clone();
        let handle = runtime.get_handle().clone();
        handle.block_on_spawned(async move {
            for _ in 0..count {
                let (tx, release) = (tx.clone(), release_all.clone());
                spawn_connection(async move {
                    tx.send(std::thread::current().id()).unwrap();
                    release.notified().await;
                });
            }
        });
        let mut per_thread = HashMap::new();
        for _ in 0..count {
            *per_thread
                .entry(rx.blocking_recv().unwrap())
                .or_default() += 1;
        }
        (per_thread, release)
    }

    trait BlockOnSpawned {
        fn block_on_spawned<F: Future<Output = ()> + Send + 'static>(&self, future: F);
    }

    impl BlockOnSpawned for Handle {
        fn block_on_spawned<F: Future<Output = ()> + Send + 'static>(&self, future: F) {
            let (done, wait) = std::sync::mpsc::channel();
            self.spawn(async move {
                future.await;
                done.send(()).unwrap();
            });
            wait.recv().unwrap();
        }
    }

    #[test]
    fn connections_spread_evenly_over_the_workers() {
        let runtime = Runtime::new_no_steal(4, "spread");
        let (per_thread, release) = spread(&runtime, 64);
        assert_eq!(per_thread.len(), 4);
        assert!(per_thread.values().all(|count| *count == 16), "{per_thread:?}");
        release.notify_waiters();
        runtime.shutdown_timeout(Duration::from_secs(1));
    }

    #[test]
    fn a_finished_connection_gives_its_slot_back() {
        let runtime = Runtime::new_no_steal(2, "slots");
        for _ in 0..2 {
            let (per_thread, release) = spread(&runtime, 10);
            assert_eq!(per_thread.values().copied().collect::<Vec<_>>(), [5, 5]);
            release.notify_waiters();
            std::thread::sleep(Duration::from_millis(100));
        }
        runtime.shutdown_timeout(Duration::from_secs(1));
    }

    #[test]
    fn an_idle_pool_keeps_connections_on_the_accepting_worker() {
        let runtime = Runtime::new_no_steal(4, "local");
        let (tx, rx) = std::sync::mpsc::channel();
        runtime.get_handle().spawn(async move {
            let caller = std::thread::current().id();
            for _ in 0..8 {
                let (done, finished) = tokio::sync::oneshot::channel();
                spawn_connection(async move {
                    done.send(std::thread::current().id()).unwrap();
                });
                assert_eq!(finished.await.unwrap(), caller);
                sleep(Duration::from_millis(1)).await;
            }
            tx.send(()).unwrap();
        });
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        runtime.shutdown_timeout(Duration::from_secs(1));
    }

    #[test]
    fn an_aged_runtime_runs_connections_on_its_executors_and_spreads_them_evenly() {
        let runtime = aged_runtime(4, "aged", Duration::from_millis(40));
        let (per_thread, release) = spread(&runtime, 64);
        assert_eq!(per_thread.len(), 4);
        assert!(per_thread.values().all(|count| *count == 16), "{per_thread:?}");
        release.notify_waiters();
        runtime.shutdown_timeout(Duration::from_secs(1));
    }

    /// A thread started after a runtime was shut down must not inherit that runtime's worker table, even when it
    /// reuses the identifier of one of its threads.
    #[test]
    fn workers_of_a_new_runtime_do_not_inherit_a_shut_down_runtime() {
        for round in 0..8 {
            let runtime = Runtime::new_no_steal(2, "reuse");
            let (per_thread, release) = spread(&runtime, 4);
            assert_eq!(per_thread.len(), 2, "round {round}: {per_thread:?}");
            release.notify_waiters();
            runtime.shutdown_timeout(Duration::from_secs(1));
        }
    }

    /// The streams of one HTTP/2 connection run on the worker that serves the connection: moving them to another
    /// worker spreads the connection's data over two cores and costs a cross-thread wake-up for every frame.
    #[test]
    fn streams_of_an_aged_runtime_run_on_the_worker_that_spawned_them() {
        let runtime = aged_runtime(4, "streams", Duration::from_millis(40));
        let (tx, rx) = std::sync::mpsc::channel();
        runtime.get_handle().spawn(async move {
            spawn_connection(async move {
                let worker = std::thread::current().id();
                for _ in 0..64 {
                    let tx = tx.clone();
                    spawn_stream(async move {
                        tx.send((worker, std::thread::current().id())).unwrap();
                    });
                }
            });
        });
        for _ in 0..64 {
            let (worker, stream) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(worker, stream);
        }
        runtime.shutdown_timeout(Duration::from_secs(1));
    }

    #[test]
    fn a_work_stealing_runtime_spawns_on_itself() {
        let runtime = Runtime::new_steal(2, "steal");
        let (tx, rx) = std::sync::mpsc::channel();
        runtime.get_handle().block_on_spawned(async move {
            spawn_connection(async move { tx.send(()).unwrap() });
        });
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }
}
