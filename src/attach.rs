use std::collections::HashMap;
use std::env;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, bail};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use socket2::{SockRef, TcpKeepalive};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use url::Url;
use uuid::Uuid;

use crate::cli::{AttachArgs, AttachMode};
use crate::contract::{
    CollectorSet, Completeness, Manifest, Mode, PlatformInfo, ProcessResult, RuntimeInfo,
    manifest_path,
};
use crate::{VERSION, analyze, report, run, util};

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = futures_util::stream::SplitSink<WsStream, Message>;
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<anyhow::Result<Value>>>>>;

#[derive(Debug, Clone)]
struct CdpEvent {
    method: String,
    params: Value,
}

struct PendingGuard {
    pending: Pending,
    id: u64,
    done: bool,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let pending = self.pending.clone();
        let id = self.id;
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                pending.lock().await.remove(&id);
            });
        }
    }
}

struct CdpClient {
    sink: Arc<Mutex<WsSink>>,
    next_id: AtomicU64,
    pending: Pending,
    events: broadcast::Sender<CdpEvent>,
    reader: tokio::task::JoinHandle<()>,
    keepalive: tokio::task::JoinHandle<()>,
}

impl Drop for CdpClient {
    fn drop(&mut self) {
        self.reader.abort();
        self.keepalive.abort();
    }
}

impl CdpClient {
    async fn connect(url: &str) -> anyhow::Result<Self> {
        let request = url
            .into_client_request()
            .context("invalid Inspector WebSocket URL")?;
        let mut config = WebSocketConfig::default();
        config.max_message_size = Some(128 << 20);
        config.max_frame_size = Some(16 << 20);
        let (stream, _) =
            tokio_tungstenite::connect_async_with_config(request, Some(config), false)
                .await
                .context("failed to connect to Inspector WebSocket")?;
        enable_tcp_keepalive(stream.get_ref());
        let (sink, mut stream) = stream.split();
        let sink = Arc::new(Mutex::new(sink));
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (events, _) = broadcast::channel(4096);
        let pending_reader = pending.clone();
        let events_reader = events.clone();
        let reader = tokio::spawn(async move {
            while let Some(message) = stream.next().await {
                let text = match message {
                    Ok(Message::Text(text)) => text.to_string(),
                    Ok(Message::Binary(bytes)) => match String::from_utf8(bytes.to_vec()) {
                        Ok(text) => text,
                        Err(_) => continue,
                    },
                    Ok(Message::Close(_)) | Err(_) => break,
                    _ => continue,
                };
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                if let Some(id) = value.get("id").and_then(Value::as_u64) {
                    if let Some(sender) = pending_reader.lock().await.remove(&id) {
                        let result = if let Some(error) = value.get("error") {
                            Err(anyhow::anyhow!("Inspector error: {error}"))
                        } else {
                            Ok(value.get("result").cloned().unwrap_or(Value::Null))
                        };
                        let _ = sender.send(result);
                    }
                } else if let Some(method) = value.get("method").and_then(Value::as_str) {
                    let _ = events_reader.send(CdpEvent {
                        method: method.to_string(),
                        params: value.get("params").cloned().unwrap_or(Value::Null),
                    });
                }
            }
            pending_reader.lock().await.clear();
        });

        let keepalive_sink = sink.clone();
        let keepalive = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                if keepalive_sink
                    .lock()
                    .await
                    .send(Message::Ping(Vec::new().into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        Ok(Self {
            sink,
            next_id: AtomicU64::new(1),
            pending,
            events,
            reader,
            keepalive,
        })
    }

    async fn command(&self, method: &str, params: Option<Value>) -> anyhow::Result<Value> {
        self.command_with_timeout(method, params, Duration::from_secs(30))
            .await
    }

    async fn command_with_timeout(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> anyhow::Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let message = serde_json::to_string(&json!({
            "id": id,
            "method": method,
            "params": params.unwrap_or_else(|| json!({})),
        }))?;
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().await.insert(id, sender);
        let mut guard = PendingGuard {
            pending: self.pending.clone(),
            id,
            done: false,
        };
        if let Err(error) = self
            .sink
            .lock()
            .await
            .send(Message::Text(message.into()))
            .await
        {
            self.pending.lock().await.remove(&id);
            guard.done = true;
            return Err(error).context("failed to send Inspector command");
        }
        let response = match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => Err(anyhow::anyhow!(
                "Inspector connection closed while awaiting {method}"
            )),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(anyhow::anyhow!("Inspector command timed out: {method}"))
            }
        };
        guard.done = true;
        response
    }

    fn subscribe(&self) -> broadcast::Receiver<CdpEvent> {
        self.events.subscribe()
    }

    #[cfg(test)]
    async fn pending_len(&self) -> usize {
        self.pending.lock().await.len()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InspectorTarget {
    web_socket_debugger_url: Option<String>,
    #[serde(default)]
    r#type: String,
}

pub async fn execute(args: AttachArgs) -> anyhow::Result<u8> {
    let websocket = resolve_websocket(&args.url, args.allow_remote_inspector).await?;
    validate_endpoint(&websocket, args.allow_remote_inspector)?;
    let client = CdpClient::connect(&websocket).await?;
    let (runtime, platform) = query_runtime(&client)
        .await
        .context("Inspector target is not a supported Node.js runtime")?;
    run::validate_runtime(&runtime)?;

    let cwd = env::current_dir()?;
    let run_id = Uuid::new_v4().to_string();
    let name = args
        .name
        .clone()
        .unwrap_or_else(|| format!("attach-{}", Utc::now().format("%Y%m%d-%H%M%S")));
    let run_dir = run::create_run_directory(&cwd, &args.output, &name, &run_id)?;
    run::create_layout(&run_dir)?;
    let started_at = Utc::now();
    let cpu = matches!(args.mode, AttachMode::Cpu | AttachMode::All);
    let heap = matches!(args.mode, AttachMode::Heap | AttachMode::All);
    let mut manifest = Manifest {
        schema_version: crate::SCHEMA_VERSION,
        v8scope_version: VERSION.into(),
        run_id: run_id.clone(),
        name,
        mode: Mode::Attach,
        collectors: CollectorSet {
            telemetry: false,
            cpu,
            heap,
            asynchronous: false,
        },
        started_at,
        finished_at: None,
        command: vec!["attach".into(), redacted_endpoint(&websocket)?],
        cwd: "<project>".into(),
        redact_paths: true,
        platform,
        runtime,
        process: ProcessResult::default(),
        completeness: Completeness::default(),
        files: Vec::new(),
    };
    util::atomic_write_json(&manifest_path(&run_dir), &manifest)?;

    if cpu {
        client.command("Profiler.enable", None).await?;
        client.command("Profiler.start", None).await?;
    }
    if heap {
        client.command("HeapProfiler.enable", None).await?;
        client
            .command(
                "HeapProfiler.startSampling",
                Some(json!({
                    "samplingInterval": 524288,
                    "stackDepth": 128,
                    "includeObjectsCollectedByMajorGC": true,
                    "includeObjectsCollectedByMinorGC": true,
                })),
            )
            .await?;
    }
    let interrupted = tokio::select! {
        _ = tokio::time::sleep(args.duration) => false,
        signal = tokio::signal::ctrl_c() => { signal?; true },
    };
    if cpu {
        let result = client.command("Profiler.stop", None).await?;
        let profile = result
            .get("profile")
            .context("Profiler.stop returned no profile")?;
        util::atomic_write_json(&run_dir.join("profiles/cpu/CPU.attach.cpuprofile"), profile)?;
        let _ = client.command("Profiler.disable", None).await;
    }
    if heap {
        let result = client.command("HeapProfiler.stopSampling", None).await?;
        let profile = result
            .get("profile")
            .context("HeapProfiler.stopSampling returned no profile")?;
        util::atomic_write_json(
            &run_dir.join("profiles/heap/Heap.attach.heapprofile"),
            profile,
        )?;
    }
    if args.heap_snapshot {
        capture_heap_snapshot(
            &client,
            &run_dir.join("profiles/heap/Heap.attach.heapsnapshot"),
        )
        .await?;
    }
    let _ = client.command("HeapProfiler.disable", None).await;

    manifest.finished_at = Some(Utc::now());
    manifest.process.interrupted = interrupted;
    util::atomic_write_json(&manifest_path(&run_dir), &manifest)?;
    analyze::reanalyze(&run_dir, true).await?;
    run::finalize_manifest(&run_dir, &mut manifest)?;
    if args.open {
        report::open(&run_dir)?;
    }
    println!("V8Scope run: {}", run_dir.display());
    Ok(if interrupted { 130 } else { 0 })
}

async fn resolve_websocket(input: &str, allow_remote: bool) -> anyhow::Result<String> {
    let url = Url::parse(input).context("invalid --url")?;
    validate_url(&url, allow_remote)?;
    if matches!(url.scheme(), "ws" | "wss") {
        return Ok(input.to_string());
    }
    if !matches!(url.scheme(), "http" | "https") {
        bail!("Inspector URL must use http, https, ws, or wss");
    }
    let mut discovery = url.clone();
    if discovery.path().is_empty() || discovery.path() == "/" {
        discovery.set_path("/json/list");
    }
    let targets: Vec<InspectorTarget> = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?
        .get(discovery)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .context("Inspector discovery response is invalid")?;
    targets
        .iter()
        .find(|target| target.r#type == "node" && target.web_socket_debugger_url.is_some())
        .or_else(|| {
            targets
                .iter()
                .find(|target| target.web_socket_debugger_url.is_some())
        })
        .and_then(|target| target.web_socket_debugger_url.clone())
        .context("Inspector discovery returned no WebSocket target")
}

fn validate_endpoint(input: &str, allow_remote: bool) -> anyhow::Result<()> {
    validate_url(&Url::parse(input)?, allow_remote)
}

fn redacted_endpoint(input: &str) -> anyhow::Result<String> {
    let url = Url::parse(input)?;
    let host = url
        .host_str()
        .context("Inspector URL has no host")?
        .trim_start_matches('[')
        .trim_end_matches(']');
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    let port = url
        .port()
        .map(|value| format!(":{value}"))
        .unwrap_or_default();
    Ok(format!("{}://{host}{port}", url.scheme()))
}

fn validate_url(url: &Url, allow_remote: bool) -> anyhow::Result<()> {
    let host = url
        .host_str()
        .context("Inspector URL has no host")?
        .trim_start_matches('[')
        .trim_end_matches(']');
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    if !loopback && !allow_remote {
        bail!("remote Inspector endpoints require --allow-remote-inspector");
    }
    Ok(())
}

async fn query_runtime(client: &CdpClient) -> anyhow::Result<(RuntimeInfo, PlatformInfo)> {
    let result = client
        .command(
            "Runtime.evaluate",
            Some(json!({
                "expression": "JSON.stringify({node:process.version,v8:process.versions.v8,platform:process.platform,arch:process.arch})",
                "returnByValue": true,
            })),
        )
        .await?;
    let value = result
        .pointer("/result/value")
        .and_then(Value::as_str)
        .context("Runtime.evaluate returned no value")?;
    let parsed: Value = serde_json::from_str(value)?;
    let runtime = RuntimeInfo {
        node: parsed
            .get("node")
            .and_then(Value::as_str)
            .map(str::to_string),
        v8: parsed.get("v8").and_then(Value::as_str).map(str::to_string),
    };
    let platform = normalize_target_platform(
        parsed
            .get("platform")
            .and_then(Value::as_str)
            .context("Node.js did not report process.platform")?,
        parsed
            .get("arch")
            .and_then(Value::as_str)
            .context("Node.js did not report process.arch")?,
    );
    Ok((runtime, platform))
}

fn normalize_target_platform(os: &str, arch: &str) -> PlatformInfo {
    PlatformInfo {
        os: match os {
            "darwin" => "macos",
            "win32" => "windows",
            value => value,
        }
        .into(),
        arch: match arch {
            "x64" => "x86_64",
            "arm64" => "aarch64",
            "ia32" => "x86",
            value => value,
        }
        .into(),
    }
}

async fn capture_heap_snapshot(client: &CdpClient, path: &Path) -> anyhow::Result<()> {
    let mut receiver = client.subscribe();
    let mut file = tokio::fs::File::create(path).await?;
    let snapshot = client.command_with_timeout(
        "HeapProfiler.takeHeapSnapshot",
        Some(json!({ "reportProgress": true })),
        Duration::from_secs(300),
    );
    tokio::pin!(snapshot);
    let mut command_done = false;
    let mut quiet_deadline: Option<tokio::time::Instant> = None;
    loop {
        tokio::select! {
            result = &mut snapshot, if !command_done => {
                result?;
                command_done = true;
                quiet_deadline = Some(tokio::time::Instant::now() + Duration::from_millis(250));
            }
            event = receiver.recv() => {
                match event {
                    Ok(event) if event.method == "HeapProfiler.addHeapSnapshotChunk" => {
                        if let Some(chunk) = event.params.get("chunk").and_then(Value::as_str) {
                            file.write_all(chunk.as_bytes()).await?;
                            if command_done {
                                quiet_deadline = Some(tokio::time::Instant::now() + Duration::from_millis(250));
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(count)) => {
                        bail!("heap snapshot event stream lagged by {count} messages");
                    }
                    Err(broadcast::error::RecvError::Closed) => bail!("Inspector closed during heap snapshot"),
                }
            }
            _ = async {
                if let Some(deadline) = quiet_deadline { tokio::time::sleep_until(deadline).await }
                else { std::future::pending::<()>().await }
            }, if command_done => break,
        }
    }
    file.flush().await?;
    Ok(())
}

fn enable_tcp_keepalive(stream: &tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>) {
    let tcp = match stream {
        tokio_tungstenite::MaybeTlsStream::Plain(stream) => stream,
        tokio_tungstenite::MaybeTlsStream::Rustls(stream) => stream.get_ref().0,
        _ => return,
    };
    let socket = SockRef::from(tcp);
    let keepalive = TcpKeepalive::new().with_time(Duration::from_secs(30));
    #[cfg(not(any(target_os = "openbsd", target_os = "haiku")))]
    let keepalive = keepalive.with_interval(Duration::from_secs(10));
    let _ = socket.set_tcp_keepalive(&keepalive);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    #[test]
    fn rejects_remote_inspector_by_default() {
        let error = validate_endpoint("ws://example.com:9229/id", false).unwrap_err();
        assert!(error.to_string().contains("--allow-remote-inspector"));
    }

    #[test]
    fn allows_loopback_inspector() {
        validate_endpoint("ws://127.0.0.1:9229/id", false).unwrap();
        validate_endpoint("ws://[::1]:9229/id", false).unwrap();
    }

    #[test]
    fn inspector_target_identifier_is_not_persisted() {
        assert_eq!(
            redacted_endpoint("ws://127.0.0.1:9229/private-target-id").unwrap(),
            "ws://127.0.0.1:9229"
        );
    }

    #[test]
    fn normalizes_node_platform_for_manifest_comparison() {
        let macos = normalize_target_platform("darwin", "arm64");
        assert_eq!(macos.os, "macos");
        assert_eq!(macos.arch, "aarch64");
        let windows = normalize_target_platform("win32", "x64");
        assert_eq!(windows.os, "windows");
        assert_eq!(windows.arch, "x86_64");
    }

    #[tokio::test]
    async fn timed_out_commands_leave_no_pending_entry() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut websocket = accept_async(stream).await.unwrap();
            let _ = websocket.next().await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        let client = CdpClient::connect(&format!("ws://{address}"))
            .await
            .unwrap();
        let error = client
            .command_with_timeout("Runtime.enable", None, Duration::from_millis(10))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert_eq!(client.pending_len().await, 0);
        server.await.unwrap();
    }
}
