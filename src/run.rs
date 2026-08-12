use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use chrono::Utc;
use goose::GooseAttack;
use goose::config::GooseConfiguration;
use goose::goose::{GooseUser, Scenario, Transaction, TransactionFunction};
use goose::prelude::TransactionResult;
#[cfg(windows)]
use process_wrap::tokio::JobObject;
#[cfg(unix)]
use process_wrap::tokio::ProcessGroup;
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use serde::Deserialize;
use tokio::sync::{mpsc, watch};
use url::Url;
use uuid::Uuid;

use crate::cli::{CleanArgs, RunArgs};
use crate::contract::{
    Artifact, CollectorSet, Completeness, Manifest, Mode, PlatformInfo, ProcessResult, RuntimeInfo,
    manifest_path,
};
use crate::{VERSION, analyze, process, report, util};

const PROBE: &[u8] = include_bytes!("../assets/probe.cjs");
const READY_TIMEOUT: Duration = Duration::from_secs(60);
const GRACEFUL_STOP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Deserialize)]
struct RuntimeProbe {
    node: String,
    v8: String,
}

pub async fn execute(mode: Mode, args: RunArgs) -> anyhow::Result<u8> {
    util::validate_node_command(&args.command)?;
    validate_args(&args)?;

    let cwd = env::current_dir()?;
    let run_id = Uuid::new_v4().to_string();
    let run_name = args
        .name
        .clone()
        .unwrap_or_else(|| format!("{}-{}", mode_name(mode), Utc::now().format("%Y%m%d-%H%M%S")));
    let run_dir = create_run_directory(&cwd, &args.output, &run_name, &run_id)?;
    create_layout(&run_dir)?;
    util::atomic_write(&run_dir.join("runtime/v8scope-probe.cjs"), PROBE)?;

    let runtime = detect_runtime(&args.command[0]).await?;
    validate_runtime(&runtime)?;
    let started_at = Utc::now();
    let mut manifest = Manifest {
        schema_version: crate::SCHEMA_VERSION,
        v8scope_version: VERSION.into(),
        run_id: run_id.clone(),
        name: run_name,
        mode,
        collectors: CollectorSet::launch(mode),
        started_at,
        finished_at: None,
        command: redact_command(&args.command, &cwd, args.redact_paths),
        cwd: if args.redact_paths {
            "<project>".into()
        } else {
            cwd.to_string_lossy().into_owned()
        },
        redact_paths: args.redact_paths,
        platform: PlatformInfo {
            os: env::consts::OS.into(),
            arch: env::consts::ARCH.into(),
        },
        runtime,
        process: ProcessResult::default(),
        completeness: Completeness::default(),
        files: Vec::new(),
    };
    util::atomic_write_json(&manifest_path(&run_dir), &manifest)?;

    let node_options = build_node_options(mode, &args, &run_dir)?;
    let telemetry_path = run_dir.join("telemetry.ndjson");
    let async_path = run_dir.join("profiles/async/events.ndjson");
    let stop_path = run_dir.join("runtime/stop");
    let existing_options = env::var("NODE_OPTIONS").unwrap_or_default();
    let process_token = Uuid::new_v4().to_string();
    let process_tracker = process::ProcessTracker::with_environment_marker(format!(
        "V8SCOPE_PROCESS_TOKEN={process_token}"
    ));

    let mut command = CommandWrap::with_new(&args.command[0], |command| {
        command.args(&args.command[1..]);
        command.env(
            "NODE_OPTIONS",
            [existing_options.as_str(), node_options.as_str()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
        );
        command.env("V8SCOPE_TELEMETRY_PATH", &telemetry_path);
        command.env("V8SCOPE_PROCESS_TOKEN", &process_token);
        #[cfg(windows)]
        command.env("V8SCOPE_STOP_PATH", &stop_path);
        command.env(
            "V8SCOPE_SAMPLE_INTERVAL_MS",
            args.sample_interval.as_millis().max(10).to_string(),
        );
        if mode.captures_async() {
            command.env("V8SCOPE_ASYNC_PATH", &async_path);
            command.env(
                "V8SCOPE_ASYNC_MAX_EVENTS",
                args.async_max_events.to_string(),
            );
        }
    });
    #[cfg(unix)]
    command.wrap(ProcessGroup::leader());
    #[cfg(windows)]
    command.wrap(JobObject);
    command.wrap(KillOnDrop);

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to launch {}", args.command[0]))?;
    let root_pid = child.id().context("spawned process has no PID")?;
    manifest.process.root_pid = Some(root_pid);
    util::atomic_write_json(&manifest_path(&run_dir), &manifest)?;

    let (sample_stop_tx, sample_stop_rx) = watch::channel(false);
    let process_task = tokio::spawn(process::sample_tree(
        root_pid,
        run_dir.join("process.ndjson"),
        args.sample_interval.max(Duration::from_millis(100)),
        sample_stop_rx,
        process_tracker.clone(),
    ));

    let has_workload = args.ready_url.is_some();
    let (workload_tx, mut workload_rx) = mpsc::channel(1);
    let workload_task = if has_workload {
        let ready_url = args.ready_url.clone().expect("clap enforces ready_url");
        let on_ready = args.on_ready.clone();
        let load = args.load_url.clone().map(|url| LoadOptions {
            url,
            users: args.connections,
            rate: args.rate,
            duration: args.load_duration,
        });
        let workload_run_dir = run_dir.clone();
        Some(tokio::spawn(async move {
            let result =
                run_workload(&ready_url, on_ready.as_deref(), load, &workload_run_dir).await;
            let _ = workload_tx.send(result).await;
        }))
    } else {
        None
    };

    enum End {
        Exited(ExitStatus),
        Interrupted,
        Duration(anyhow::Result<()>),
        Workload(anyhow::Result<()>),
    }

    let duration_telemetry = telemetry_path.clone();
    let duration_wait = async {
        if let Some(duration) = args.duration {
            wait_for_probe_start(&duration_telemetry).await?;
            tokio::time::sleep(duration).await;
        } else {
            std::future::pending::<()>().await;
        }
        Ok::<(), anyhow::Error>(())
    };
    let workload_wait = async {
        if has_workload {
            workload_rx.recv().await.unwrap_or_else(|| Ok(()))
        } else {
            std::future::pending::<anyhow::Result<()>>().await
        }
    };
    tokio::pin!(duration_wait);
    tokio::pin!(workload_wait);

    let end = tokio::select! {
        biased;
        status = child.wait() => End::Exited(status?),
        signal = tokio::signal::ctrl_c() => {
            signal.context("failed to listen for Ctrl+C")?;
            End::Interrupted
        },
        result = &mut workload_wait => End::Workload(result),
        result = &mut duration_wait => End::Duration(result),
    };

    let mut workload_error = None;
    let (status, interrupted, requested_code, mut forced_stop) = match end {
        End::Exited(status) => {
            if has_workload {
                workload_error = Some("target exited before workload completed".into());
            }
            let forced = settle_descendants(&mut child, root_pid).await?;
            (status, false, None, forced)
        }
        End::Interrupted => {
            let stopped = stop_child(&mut child, root_pid, &stop_path).await?;
            (stopped.status, true, Some(130), stopped.forced)
        }
        End::Duration(result) => {
            if let Err(error) = result {
                workload_error = Some(format!("profiler startup failed: {error:#}"));
            } else if has_workload {
                workload_error =
                    Some("profiling duration elapsed before workload completed".into());
            }
            let stopped = stop_child(&mut child, root_pid, &stop_path).await?;
            (stopped.status, true, Some(0), stopped.forced)
        }
        End::Workload(result) => {
            if let Err(error) = result {
                workload_error = Some(format!("workload failed: {error:#}"));
            }
            let stopped = stop_child(&mut child, root_pid, &stop_path).await?;
            (stopped.status, true, Some(0), stopped.forced)
        }
    };

    if let Some(task) = workload_task {
        task.abort();
        let _ = task.await;
    }
    process::record_telemetry_processes(&telemetry_path, &process_tracker)?;
    forced_stop |=
        process::settle_tracked_processes(&process_tracker, root_pid, GRACEFUL_STOP_TIMEOUT)
            .await?;
    let _ = sample_stop_tx.send(true);
    if let Ok(Err(error)) = process_task.await {
        manifest
            .completeness
            .warnings
            .push(format!("process sampling failed: {error:#}"));
    }

    manifest.finished_at = Some(Utc::now());
    manifest.process.exit_code = status.code();
    manifest.process.signal = exit_signal(&status);
    manifest.process.interrupted = interrupted;
    if forced_stop {
        manifest.completeness.partial = true;
        manifest.completeness.warnings.push(
            "graceful shutdown timed out; remaining process tree was forcibly terminated".into(),
        );
    }
    if let Some(error) = workload_error.clone() {
        manifest.completeness.warnings.push(error);
        manifest.completeness.partial = true;
    }
    util::atomic_write_json(&manifest_path(&run_dir), &manifest)?;

    let analysis_failed = if let Err(error) = analyze::reanalyze(&run_dir, !args.no_report).await {
        manifest
            .completeness
            .warnings
            .push(format!("analysis failed: {error:#}"));
        manifest.completeness.partial = true;
        true
    } else {
        false
    };
    finalize_manifest(&run_dir, &mut manifest)?;

    if args.open && !args.no_report {
        report::open(&run_dir)?;
    }

    println!("V8Scope run: {}", run_dir.display());
    if workload_error.is_some() {
        return Ok(70);
    }
    if analysis_failed || manifest.completeness.partial {
        return Ok(70);
    }
    Ok(requested_code.unwrap_or_else(|| exit_code(&status, interrupted)))
}

fn validate_args(args: &RunArgs) -> anyhow::Result<()> {
    if args.sample_interval < Duration::from_millis(10) {
        bail!("--sample-interval must be at least 10ms");
    }
    if args.cpu_interval == 0 {
        bail!("--cpu-interval must be greater than zero");
    }
    if args.heap_interval == 0 {
        bail!("--heap-interval must be greater than zero");
    }
    let existing = env::var("NODE_OPTIONS").unwrap_or_default();
    for conflict in ["--cpu-prof", "--heap-prof", "V8SCOPE_TELEMETRY_PATH"] {
        if existing.contains(conflict) {
            bail!("NODE_OPTIONS already contains a conflicting profiler option: {conflict}");
        }
    }
    Ok(())
}

pub(crate) fn create_run_directory(
    cwd: &Path,
    output: &Path,
    name: &str,
    run_id: &str,
) -> anyhow::Result<PathBuf> {
    let output = if output.is_absolute() {
        output.to_path_buf()
    } else {
        cwd.join(output)
    };
    std::fs::create_dir_all(&output)?;
    let directory = output.join(format!("{}-{}", util::safe_name(name), &run_id[..8]));
    std::fs::create_dir(&directory)
        .with_context(|| format!("failed to create run directory {}", directory.display()))?;
    directory
        .canonicalize()
        .context("failed to resolve run directory")
}

pub(crate) fn create_layout(run_dir: &Path) -> anyhow::Result<()> {
    for directory in [
        "runtime",
        "profiles/cpu",
        "profiles/heap",
        "profiles/async",
        "report/assets",
    ] {
        std::fs::create_dir_all(run_dir.join(directory))?;
    }
    Ok(())
}

fn build_node_options(mode: Mode, args: &RunArgs, run_dir: &Path) -> anyhow::Result<String> {
    let probe = util::node_option(&run_dir.join("runtime/v8scope-probe.cjs"));
    let mut options = vec![format!("--require={probe}")];
    if mode.captures_cpu() {
        options.push("--cpu-prof".into());
        options.push(format!(
            "--cpu-prof-dir={}",
            util::node_option(&run_dir.join("profiles/cpu"))
        ));
        options.push(format!("--cpu-prof-interval={}", args.cpu_interval));
    }
    if mode.captures_heap() {
        options.push("--heap-prof".into());
        options.push(format!(
            "--heap-prof-dir={}",
            util::node_option(&run_dir.join("profiles/heap"))
        ));
        options.push(format!("--heap-prof-interval={}", args.heap_interval));
    }
    Ok(options.join(" "))
}

async fn detect_runtime(node: &str) -> anyhow::Result<RuntimeInfo> {
    let output = tokio::process::Command::new(node)
        .args([
            "-p",
            "JSON.stringify({node:process.version,v8:process.versions.v8})",
        ])
        .output()
        .await?;
    if !output.status.success() {
        bail!("failed to query Node.js runtime");
    }
    let probe: RuntimeProbe = serde_json::from_slice(&output.stdout)?;
    Ok(RuntimeInfo {
        node: Some(probe.node),
        v8: Some(probe.v8),
    })
}

pub(crate) fn validate_runtime(runtime: &RuntimeInfo) -> anyhow::Result<()> {
    let version = runtime
        .node
        .as_deref()
        .context("Node.js did not report its version")?;
    let major = version
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .context("Node.js reported an invalid version")?;
    if !matches!(major, 22 | 24 | 26) {
        bail!("V8Scope supports Node.js 22, 24, and 26; received {version}");
    }
    Ok(())
}

struct StopOutcome {
    status: ExitStatus,
    forced: bool,
}

async fn stop_child(
    child: &mut Box<dyn ChildWrapper>,
    root_pid: u32,
    _stop_path: &Path,
) -> anyhow::Result<StopOutcome> {
    #[cfg(unix)]
    let interrupted = process::interrupt_group(root_pid);
    #[cfg(windows)]
    let interrupted = util::atomic_write(_stop_path, b"stop\n");
    if interrupted.is_ok()
        && let Ok(status) = tokio::time::timeout(GRACEFUL_STOP_TIMEOUT, child.wait()).await
    {
        let status = status?;
        let forced = settle_descendants(child, root_pid).await?;
        #[cfg(windows)]
        let _ = std::fs::remove_file(_stop_path);
        return Ok(StopOutcome { status, forced });
    }
    child.start_kill()?;
    let status = child
        .wait()
        .await
        .context("failed to reap target process")?;
    let _ = settle_descendants(child, root_pid).await?;
    #[cfg(windows)]
    let _ = std::fs::remove_file(_stop_path);
    Ok(StopOutcome {
        status,
        forced: true,
    })
}

async fn wait_for_probe_start(path: &Path) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(content) = tokio::fs::read_to_string(path).await
            && content
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .any(|value| value.get("event").and_then(|event| event.as_str()) == Some("start"))
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("probe did not emit a start event within 5s");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn settle_descendants(
    child: &mut Box<dyn ChildWrapper>,
    root_pid: u32,
) -> anyhow::Result<bool> {
    #[cfg(unix)]
    {
        let _ = child;
        process::settle_group(root_pid, GRACEFUL_STOP_TIMEOUT).await
    }
    #[cfg(windows)]
    {
        let _ = (child, root_pid);
        Ok(false)
    }
}

#[derive(Clone)]
struct LoadOptions {
    url: String,
    users: usize,
    rate: Option<u64>,
    duration: Duration,
}

async fn run_workload(
    ready_url: &str,
    on_ready: Option<&str>,
    load: Option<LoadOptions>,
    run_dir: &Path,
) -> anyhow::Result<()> {
    wait_until_ready(ready_url).await?;
    if let Some(command) = on_ready {
        run_shell(command, ready_url, run_dir).await?;
    }
    if let Some(load) = load {
        run_goose(load, run_dir).await?;
    }
    Ok(())
}

async fn wait_until_ready(url: &str) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(1))
        .timeout(Duration::from_secs(2))
        .build()?;
    let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
    loop {
        if let Ok(response) = client.get(url).send().await
            && (response.status().is_success() || response.status().is_redirection())
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "target did not become ready at {url} within {}s",
                READY_TIMEOUT.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn run_shell(command: &str, ready_url: &str, run_dir: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    let (shell, shell_arg) = ("sh", "-c");
    #[cfg(windows)]
    let (shell, shell_arg) = ("cmd", "/C");
    let process_token = Uuid::new_v4().to_string();
    let shell_tracker = process::ProcessTracker::with_environment_marker(format!(
        "V8SCOPE_PROCESS_TOKEN={process_token}"
    ));
    let mut process = CommandWrap::with_new(shell, |process| {
        process.args([shell_arg, command]);
        process.env("V8SCOPE_URL", ready_url);
        process.env("V8SCOPE_RUN_DIR", run_dir);
        process.env("V8SCOPE_PROCESS_TOKEN", &process_token);
    });
    #[cfg(unix)]
    process.wrap(ProcessGroup::leader());
    #[cfg(windows)]
    process.wrap(JobObject);
    process.wrap(KillOnDrop);
    let mut child = process
        .spawn()
        .context("failed to launch --on-ready command")?;
    let shell_pid = child
        .id()
        .context("--on-ready command started without a PID")?;
    #[cfg(unix)]
    let _group_guard = ShellGroupGuard::new(shell_pid);
    let (monitor_stop_tx, monitor_stop_rx) = watch::channel(false);
    let monitor_task = tokio::spawn(process::monitor_tree(
        shell_pid,
        shell_tracker.clone(),
        monitor_stop_rx,
    ));
    let status = child.wait().await?;
    let _ = child.start_kill();
    let _ =
        process::settle_tracked_processes(&shell_tracker, shell_pid, Duration::from_millis(250))
            .await?;
    let _ = monitor_stop_tx.send(true);
    let _ = monitor_task.await;
    if !status.success() {
        bail!("--on-ready command exited with {status}");
    }
    Ok(())
}

#[cfg(unix)]
struct ShellGroupGuard {
    pid: u32,
    armed: bool,
}

#[cfg(unix)]
impl ShellGroupGuard {
    fn new(pid: u32) -> Self {
        Self { pid, armed: true }
    }
}

#[cfg(unix)]
impl Drop for ShellGroupGuard {
    fn drop(&mut self) {
        if self.armed {
            use nix::sys::signal::{Signal, killpg};
            use nix::unistd::Pid;
            let _ = killpg(Pid::from_raw(self.pid as i32), Signal::SIGKILL);
        }
    }
}

async fn run_goose(load: LoadOptions, run_dir: &Path) -> anyhow::Result<()> {
    let parsed = Url::parse(&load.url).context("invalid --load-url")?;
    let host = format!(
        "{}://{}{}",
        parsed.scheme(),
        parsed.host_str().context("load URL has no host")?,
        parsed
            .port()
            .map(|port| format!(":{port}"))
            .unwrap_or_default()
    );
    let path = match parsed.query() {
        Some(query) => format!("{}?{query}", parsed.path()),
        None => parsed.path().to_string(),
    };
    let mut configuration = GooseConfiguration::default();
    configuration.host = host;
    configuration.users = Some(load.users.max(1));
    configuration.hatch_rate = Some(load.users.max(1).to_string());
    configuration.run_time = format!("{}s", load.duration.as_secs().max(1));
    configuration.no_telnet = true;
    configuration.no_websocket = true;
    configuration.no_print_metrics = true;
    configuration.no_error_summary = true;
    configuration.throttle_requests = load.rate.unwrap_or(0) as usize;
    configuration.report_file = vec![
        run_dir
            .join("load-report.json")
            .to_string_lossy()
            .into_owned(),
    ];

    let path = Arc::new(path);
    let transaction: TransactionFunction = Arc::new(move |user: &mut GooseUser| {
        let path = path.clone();
        Box::pin(async move {
            user.get(path.as_str()).await?;
            Ok(()) as TransactionResult
        })
    });
    let scenario = Scenario::new("V8ScopeLoad").register_transaction(Transaction::new(transaction));
    let metrics = GooseAttack::initialize_with_config(configuration)?
        .register_scenario(scenario)
        .execute()
        .await?;
    let summary = serde_json::json!({
        "duration_s": metrics.duration,
        "maximum_users": metrics.maximum_users,
        "total_users": metrics.total_users,
        "requests": metrics.requests,
    });
    util::atomic_write_json(&run_dir.join("load-summary.json"), &summary)?;
    Ok(())
}

pub(crate) fn finalize_manifest(run_dir: &Path, manifest: &mut Manifest) -> anyhow::Result<()> {
    let telemetry = analyze::telemetry_integrity(&run_dir.join("telemetry.ndjson"))?;
    manifest.completeness.telemetry = telemetry.complete;
    if manifest.collectors.telemetry
        && let Some(warning) = telemetry.warning
        && !manifest.completeness.warnings.contains(&warning)
    {
        manifest.completeness.warnings.push(warning);
    }
    manifest.completeness.cpu = has_extension(&run_dir.join("profiles/cpu"), "cpuprofile");
    manifest.completeness.heap = has_extension(&run_dir.join("profiles/heap"), "heapprofile");
    let asynchronous = analyze::async_integrity(
        &run_dir.join("profiles/async/events.ndjson"),
        &run_dir.join("telemetry.ndjson"),
    )?;
    manifest.completeness.asynchronous = asynchronous.complete;
    if manifest.collectors.asynchronous
        && let Some(warning) = asynchronous.warning
        && !manifest.completeness.warnings.contains(&warning)
    {
        manifest.completeness.warnings.push(warning);
    }
    manifest.completeness.partial = manifest.completeness.partial
        || (manifest.collectors.telemetry && !manifest.completeness.telemetry)
        || (manifest.collectors.cpu && !manifest.completeness.cpu)
        || (manifest.collectors.heap && !manifest.completeness.heap)
        || (manifest.collectors.asynchronous && !manifest.completeness.asynchronous);
    manifest.files.clear();
    for relative in util::collect_files(run_dir)? {
        let path = run_dir.join(&relative);
        let (bytes, sha256) = util::sha256_file(&path)?;
        manifest.files.push(Artifact {
            kind: artifact_kind(&relative),
            path: relative.to_string_lossy().replace('\\', "/"),
            bytes,
            sha256,
        });
    }
    util::atomic_write_json(&manifest_path(run_dir), manifest)
}

fn redact_command(command: &[String], cwd: &Path, enabled: bool) -> Vec<String> {
    if !enabled {
        return command.to_vec();
    }
    let _ = cwd;
    let mut redacted = command
        .first()
        .and_then(|executable| Path::new(executable).file_name())
        .map(|executable| executable.to_string_lossy().into_owned())
        .into_iter()
        .collect::<Vec<_>>();
    if command.len() > 1 {
        redacted.push(format!("<{} target arguments redacted>", command.len() - 1));
    }
    redacted
}

fn exit_signal(status: &ExitStatus) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal().map(|signal| match signal {
            2 => "SIGINT".into(),
            9 => "SIGKILL".into(),
            15 => "SIGTERM".into(),
            value => format!("SIG{value}"),
        })
    }
    #[cfg(windows)]
    {
        let _ = status;
        None
    }
}

fn has_extension(directory: &Path, extension: &str) -> bool {
    std::fs::read_dir(directory)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .any(|entry| entry.path().extension().and_then(|value| value.to_str()) == Some(extension))
}

fn artifact_kind(path: &Path) -> String {
    match path.extension().and_then(|value| value.to_str()) {
        Some("cpuprofile") => "v8_cpu_profile",
        Some("heapprofile") => "v8_heap_profile",
        Some("ndjson") => "telemetry",
        Some("html") => "report",
        Some("json") => "json",
        _ => "asset",
    }
    .into()
}

fn exit_code(status: &ExitStatus, interrupted: bool) -> u8 {
    status
        .code()
        .map(|code| match code {
            0 => 0,
            1..=255 => code as u8,
            _ => 1,
        })
        .unwrap_or(if interrupted { 130 } else { 1 })
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Diagnose => "diagnose",
        Mode::Cpu => "cpu",
        Mode::Heap => "heap",
        Mode::Async => "async",
        Mode::All => "all",
        Mode::Attach => "attach",
    }
}

pub async fn clean(args: CleanArgs) -> anyhow::Result<u8> {
    if !args.output.exists() {
        return Ok(0);
    }
    let mut directories: Vec<_> = std::fs::read_dir(&args.output)?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir() && entry.path().join("manifest.json").is_file())
        .collect();
    directories.sort_by_key(|entry| {
        entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
    });
    let remove = directories.len().saturating_sub(args.keep);
    for entry in directories.into_iter().take(remove) {
        std::fs::remove_dir_all(entry.path())?;
        println!("Removed {}", entry.path().display());
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn clean_targets_only_v8scope_run_directories() {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("run");
        let unrelated = root.path().join("unrelated");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::create_dir_all(&unrelated).unwrap();
        std::fs::write(run.join("manifest.json"), "{}\n").unwrap();
        std::fs::write(unrelated.join("keep.txt"), "keep\n").unwrap();

        clean(CleanArgs {
            output: root.path().to_path_buf(),
            keep: 0,
        })
        .await
        .unwrap();

        assert!(!run.exists());
        assert!(unrelated.join("keep.txt").is_file());
    }

    #[test]
    fn accepts_supported_node_lines_and_rejects_older_runtimes() {
        assert!(
            validate_runtime(&RuntimeInfo {
                node: Some("v22.0.0".into()),
                v8: Some("12.4".into()),
            })
            .is_ok()
        );
        let error = validate_runtime(&RuntimeInfo {
            node: Some("v20.19.0".into()),
            v8: Some("11.3".into()),
        })
        .unwrap_err();
        assert!(error.to_string().contains("Node.js 22, 24, and 26"));
        assert!(
            validate_runtime(&RuntimeInfo {
                node: Some("v24.0.0".into()),
                v8: Some("13.6".into()),
            })
            .is_ok()
        );
        assert!(
            validate_runtime(&RuntimeInfo {
                node: Some("v26.0.0".into()),
                v8: Some("14.2".into()),
            })
            .is_ok()
        );
        assert!(
            validate_runtime(&RuntimeInfo {
                node: Some("v25.0.0".into()),
                v8: Some("14.1".into()),
            })
            .is_err()
        );
    }
}
