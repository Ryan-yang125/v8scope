use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use assert_cmd::cargo::cargo_bin_cmd;
use chrono::Utc;
use serde_json::Value;
use tempfile::TempDir;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn only_run(root: &Path) -> PathBuf {
    let runs = std::fs::read_dir(root)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.join("manifest.json").is_file())
        .collect::<Vec<_>>();
    assert_eq!(runs.len(), 1, "expected one run in {}", root.display());
    runs[0].clone()
}

fn json(path: &Path) -> Value {
    serde_json::from_reader(File::open(path).unwrap()).unwrap()
}

fn assert_manifest_integrity(run: &Path) {
    let manifest = json(&run.join("manifest.json"));
    let listed = manifest["files"].as_array().unwrap();
    let actual = v8scope::util::collect_files(run).unwrap();
    assert_eq!(listed.len(), actual.len(), "artifact inventory is stale");
    for artifact in listed {
        let relative = artifact["path"].as_str().unwrap();
        let (bytes, sha256) = v8scope::util::sha256_file(&run.join(relative)).unwrap();
        assert_eq!(artifact["bytes"], bytes);
        assert_eq!(artifact["sha256"], sha256);
    }
}

fn write_synthetic_run(run: &Path, summary: &v8scope::contract::Summary) {
    use v8scope::contract::{
        Artifact, CollectorSet, Comparability, Completeness, Manifest, Mode, PlatformInfo,
        ProcessResult, RuntimeInfo,
    };

    std::fs::create_dir_all(run).unwrap();
    let run_id = run.file_name().unwrap().to_string_lossy().into_owned();
    let mut summary = summary.clone();
    summary.schema_version = v8scope::SCHEMA_VERSION;
    summary.run_id.clone_from(&run_id);
    summary.comparability = Comparability {
        node_major: Some(22),
        v8_major: Some(12),
        os: "test-os".into(),
        arch: "test-arch".into(),
    };
    v8scope::util::atomic_write_json(&run.join("summary.json"), &summary).unwrap();
    v8scope::util::atomic_write(
        &run.join("telemetry.ndjson"),
        b"{\"event\":\"start\",\"pid\":1,\"thread_id\":0}\n{\"event\":\"finish\",\"pid\":1,\"thread_id\":0}\n",
    )
    .unwrap();
    let (summary_bytes, summary_sha256) =
        v8scope::util::sha256_file(&run.join("summary.json")).unwrap();
    let (telemetry_bytes, telemetry_sha256) =
        v8scope::util::sha256_file(&run.join("telemetry.ndjson")).unwrap();
    let now = Utc::now();
    let manifest = Manifest {
        schema_version: v8scope::SCHEMA_VERSION,
        v8scope_version: v8scope::VERSION.into(),
        run_id: run_id.clone(),
        name: run.file_name().unwrap().to_string_lossy().into_owned(),
        mode: Mode::Diagnose,
        collectors: CollectorSet {
            telemetry: true,
            ..Default::default()
        },
        started_at: now,
        finished_at: Some(now),
        command: vec!["node".into(), "fixture.cjs".into()],
        cwd: "<project>".into(),
        redact_paths: true,
        platform: PlatformInfo {
            os: "test-os".into(),
            arch: "test-arch".into(),
        },
        runtime: RuntimeInfo {
            node: Some("v22.0.0".into()),
            v8: Some("12.0.0".into()),
        },
        process: ProcessResult::default(),
        completeness: Completeness {
            telemetry: true,
            partial: false,
            ..Default::default()
        },
        files: vec![
            Artifact {
                path: "summary.json".into(),
                kind: "json".into(),
                bytes: summary_bytes,
                sha256: summary_sha256,
            },
            Artifact {
                path: "telemetry.ndjson".into(),
                kind: "telemetry".into(),
                bytes: telemetry_bytes,
                sha256: telemetry_sha256,
            },
        ],
    };
    v8scope::util::atomic_write_json(&run.join("manifest.json"), &manifest).unwrap();
}

#[test]
fn diagnose_collects_real_v8_cpu_profile_and_offline_report() {
    let output = TempDir::new().unwrap();
    cargo_bin_cmd!("v8scope")
        .args(["diagnose", "--output"])
        .arg(output.path())
        .args(["--", "node"])
        .arg(fixture("workload.cjs"))
        .assert()
        .success();

    let run = only_run(output.path());
    let manifest = json(&run.join("manifest.json"));
    let summary = json(&run.join("summary.json"));
    assert_eq!(manifest["completeness"]["cpu"], true);
    assert_eq!(manifest["completeness"]["partial"], false);
    assert!(summary["cpu"]["profile_samples"].as_u64().unwrap() > 0);
    assert!(!summary["cpu"]["hotspots"].as_array().unwrap().is_empty());
    assert!(run.join("report/index.html").is_file());
    assert!(run.join("report/assets/cpu-flamegraph.svg").is_file());
    assert_manifest_integrity(&run);

    cargo_bin_cmd!("v8scope")
        .arg("analyze")
        .arg(&run)
        .assert()
        .success();
    assert_manifest_integrity(&run);
    let report = std::fs::read_to_string(run.join("report/index.html")).unwrap();
    assert!(!report.contains(env!("CARGO_MANIFEST_DIR")));
    let manifest_text = std::fs::read_to_string(run.join("manifest.json")).unwrap();
    assert!(!manifest_text.contains(env!("CARGO_MANIFEST_DIR")));
}

#[test]
fn application_signal_handler_finishes_async_cleanup() {
    let output = TempDir::new().unwrap();
    let marker_root = TempDir::new().unwrap();
    let marker = marker_root.path().join("clean.txt");
    cargo_bin_cmd!("v8scope")
        .args(["cpu", "--duration", "500ms", "--output"])
        .arg(output.path())
        .args(["--", "node"])
        .arg(fixture("graceful-signal.cjs"))
        .arg(&marker)
        .assert()
        .success();
    assert!(
        marker.is_file(),
        "application SIGINT cleanup was interrupted"
    );
}

#[cfg(unix)]
#[test]
fn natural_root_exit_reaps_profiled_descendants_before_finalize() {
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;

    let output = TempDir::new().unwrap();
    let marker_root = TempDir::new().unwrap();
    let pid_file = marker_root.path().join("child.pid");
    cargo_bin_cmd!("v8scope")
        .args(["cpu", "--output"])
        .arg(output.path())
        .args(["--", "node"])
        .arg(fixture("descendant.cjs"))
        .arg(&pid_file)
        .assert()
        .success();
    let child_pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .parse::<i32>()
        .unwrap();
    assert_eq!(kill(Pid::from_raw(child_pid), None), Err(Errno::ESRCH));
    let run = only_run(output.path());
    let profiles = std::fs::read_dir(run.join("profiles/cpu"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.path().extension().and_then(|value| value.to_str()) == Some("cpuprofile")
        })
        .count();
    assert!(profiles >= 2);
    assert_manifest_integrity(&run);
}

#[cfg(unix)]
#[test]
fn duration_stop_reaps_descendants_after_root_exits_first() {
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;

    let output = TempDir::new().unwrap();
    let marker_root = TempDir::new().unwrap();
    let pid_file = marker_root.path().join("child.pid");
    cargo_bin_cmd!("v8scope")
        .args(["cpu", "--duration", "500ms", "--output"])
        .arg(output.path())
        .args(["--", "node"])
        .arg(fixture("stubborn-descendant.cjs"))
        .arg(&pid_file)
        .assert()
        .code(70);
    let child_pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .parse::<i32>()
        .unwrap();
    assert_eq!(kill(Pid::from_raw(child_pid), None), Err(Errno::ESRCH));
    let run = only_run(output.path());
    assert_eq!(
        json(&run.join("manifest.json"))["completeness"]["partial"],
        true
    );
    assert_manifest_integrity(&run);
}

#[test]
fn target_exit_before_readiness_is_a_failed_run() {
    let output = TempDir::new().unwrap();
    cargo_bin_cmd!("v8scope")
        .args(["diagnose", "--output"])
        .arg(output.path())
        .args([
            "--ready-url",
            "http://127.0.0.1:9/ready",
            "--load-url",
            "http://127.0.0.1:9/load",
            "--",
            "node",
        ])
        .arg(fixture("early-exit.cjs"))
        .assert()
        .code(70);
    let manifest = json(&only_run(output.path()).join("manifest.json"));
    assert_eq!(manifest["completeness"]["partial"], true);
    assert!(
        manifest["completeness"]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value
                .as_str()
                .unwrap()
                .contains("before workload completed"))
    );
}

#[test]
fn duration_before_readiness_marks_workload_incomplete() {
    let output = TempDir::new().unwrap();
    cargo_bin_cmd!("v8scope")
        .args(["diagnose", "--duration", "700ms", "--output"])
        .arg(output.path())
        .args([
            "--ready-url",
            "http://127.0.0.1:9/ready",
            "--load-url",
            "http://127.0.0.1:9/load",
            "--",
            "node",
            "-e",
            "setInterval(() => {}, 1000)",
        ])
        .assert()
        .code(70);

    let run = only_run(output.path());
    let manifest = json(&run.join("manifest.json"));
    assert_eq!(manifest["completeness"]["partial"], true);
    assert!(!run.join("load-summary.json").exists());
    assert!(
        manifest["completeness"]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value
                .as_str()
                .unwrap()
                .contains("duration elapsed before workload completed"))
    );
}

#[test]
fn readiness_only_is_an_enforced_workload_gate() {
    let output = TempDir::new().unwrap();
    cargo_bin_cmd!("v8scope")
        .args(["diagnose", "--duration", "300ms", "--output"])
        .arg(output.path())
        .args([
            "--ready-url",
            "http://127.0.0.1:9/ready",
            "--",
            "node",
            "-e",
            "setInterval(() => {}, 1000)",
        ])
        .assert()
        .code(70);
    let manifest = json(&only_run(output.path()).join("manifest.json"));
    assert_eq!(manifest["completeness"]["partial"], true);
    assert!(
        manifest["completeness"]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value
                .as_str()
                .unwrap()
                .contains("duration elapsed before workload completed"))
    );
}

#[cfg(unix)]
#[test]
fn cancelled_on_ready_command_cannot_escape_the_run() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let ready_url = format!("http://127.0.0.1:{port}/health");
    let output = TempDir::new().unwrap();
    let marker_root = TempDir::new().unwrap();
    let marker = marker_root.path().join("escaped.txt");
    let command = format!("sleep 2; touch '{}'", marker.display());

    cargo_bin_cmd!("v8scope")
        .args(["diagnose", "--duration", "500ms", "--output"])
        .arg(output.path())
        .args([
            "--ready-url",
            &ready_url,
            "--on-ready",
            &command,
            "--",
            "node",
        ])
        .arg(fixture("server.cjs"))
        .arg(port.to_string())
        .assert()
        .code(70);

    std::thread::sleep(std::time::Duration::from_millis(2200));
    assert!(!marker.exists(), "cancelled --on-ready command escaped");
}

#[cfg(unix)]
#[test]
fn successful_on_ready_command_cannot_leave_background_writers() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let ready_url = format!("http://127.0.0.1:{port}/health");
    let output = TempDir::new().unwrap();
    let command = "(sleep 2; echo late > \"$V8SCOPE_RUN_DIR/late.txt\") &";

    cargo_bin_cmd!("v8scope")
        .args(["diagnose", "--output"])
        .arg(output.path())
        .args([
            "--ready-url",
            &ready_url,
            "--on-ready",
            command,
            "--",
            "node",
        ])
        .arg(fixture("server.cjs"))
        .arg(port.to_string())
        .assert()
        .success();

    let run = only_run(output.path());
    std::thread::sleep(std::time::Duration::from_millis(2200));
    assert!(!run.join("late.txt").exists(), "background writer escaped");
    assert_manifest_integrity(&run);
}

#[cfg(unix)]
#[test]
fn on_ready_descendant_in_a_new_process_group_cannot_escape() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let ready_url = format!("http://127.0.0.1:{port}/health");
    let output = TempDir::new().unwrap();
    let command = format!("node '{}'", fixture("on-ready-detach.cjs").display());

    cargo_bin_cmd!("v8scope")
        .args(["diagnose", "--output"])
        .arg(output.path())
        .args([
            "--ready-url",
            &ready_url,
            "--on-ready",
            &command,
            "--",
            "node",
        ])
        .arg(fixture("server.cjs"))
        .arg(port.to_string())
        .assert()
        .success();

    let run = only_run(output.path());
    std::thread::sleep(std::time::Duration::from_millis(2200));
    assert!(
        !run.join("detached-late.txt").exists(),
        "detached --on-ready writer escaped"
    );
    assert_manifest_integrity(&run);
}

#[cfg(unix)]
#[test]
fn detached_target_descendant_is_settled_before_finalize() {
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;

    let output = TempDir::new().unwrap();
    let marker_root = TempDir::new().unwrap();
    let pid_file = marker_root.path().join("detached.pid");
    cargo_bin_cmd!("v8scope")
        .args(["cpu", "--output"])
        .arg(output.path())
        .args(["--", "node"])
        .arg(fixture("detached-descendant.cjs"))
        .arg(&pid_file)
        .assert()
        .success();

    let child_pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .parse::<i32>()
        .unwrap();
    assert_eq!(kill(Pid::from_raw(child_pid), None), Err(Errno::ESRCH));
    let run = only_run(output.path());
    let profiles = std::fs::read_dir(run.join("profiles/cpu"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.path().extension().and_then(|value| value.to_str()) == Some("cpuprofile")
        })
        .count();
    assert!(profiles >= 2);
    assert_eq!(
        json(&run.join("manifest.json"))["completeness"]["partial"],
        false
    );
    std::thread::sleep(std::time::Duration::from_millis(350));
    assert_manifest_integrity(&run);
}

#[test]
fn forced_target_death_marks_telemetry_partial_and_records_signal() {
    let output = TempDir::new().unwrap();
    cargo_bin_cmd!("v8scope")
        .args(["diagnose", "--output"])
        .arg(output.path())
        .args([
            "--",
            "node",
            "-e",
            "setTimeout(() => process.kill(process.pid, 'SIGKILL'), 100)",
        ])
        .assert()
        .failure();
    let manifest = json(&only_run(output.path()).join("manifest.json"));
    assert_eq!(manifest["completeness"]["partial"], true);
    assert_eq!(manifest["completeness"]["telemetry"], false);
    #[cfg(unix)]
    assert_eq!(manifest["process"]["signal"], "SIGKILL");
}

#[test]
fn all_collects_cpu_heap_and_explicit_async_causality() {
    let output = TempDir::new().unwrap();
    cargo_bin_cmd!("v8scope")
        .args(["all", "--output"])
        .arg(output.path())
        .args([
            "--async-max-events",
            "100000",
            "--heap-interval",
            "1024",
            "--",
            "node",
        ])
        .arg(fixture("workload.cjs"))
        .assert()
        .success();

    let run = only_run(output.path());
    let manifest = json(&run.join("manifest.json"));
    let summary = json(&run.join("summary.json"));
    for profile in ["cpu", "heap", "asynchronous"] {
        assert_eq!(manifest["completeness"][profile], true, "{profile}");
    }
    assert_eq!(summary["asynchronous"]["enabled"], true);
    assert!(summary["asynchronous"]["events"].as_u64().unwrap() > 0);
    assert!(
        !summary["memory"]["allocation_hotspots"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn worker_threads_are_captured_without_application_changes() {
    let output = TempDir::new().unwrap();
    cargo_bin_cmd!("v8scope")
        .args(["cpu", "--output"])
        .arg(output.path())
        .args(["--", "node"])
        .arg(fixture("worker.cjs"))
        .assert()
        .success();

    let run = only_run(output.path());
    let profiles = std::fs::read_dir(run.join("profiles/cpu"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.path().extension().and_then(|value| value.to_str()) == Some("cpuprofile")
        })
        .count();
    assert!(
        profiles >= 2,
        "expected profiles for the main thread and worker"
    );
}

#[test]
fn worker_async_completeness_requires_every_isolate_summary() {
    let output = TempDir::new().unwrap();
    cargo_bin_cmd!("v8scope")
        .args(["all", "--output"])
        .arg(output.path())
        .args(["--", "node"])
        .arg(fixture("worker.cjs"))
        .assert()
        .success();

    let run = only_run(output.path());
    let telemetry = std::fs::read_to_string(run.join("telemetry.ndjson")).unwrap();
    let async_path = run.join("profiles/async/events.ndjson");
    let async_events = std::fs::read_to_string(&async_path).unwrap();
    let identities = |content: &str, event: &str| {
        content
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|value| value["event"] == event)
            .map(|value| {
                (
                    value["pid"].as_u64().unwrap(),
                    value["thread_id"].as_u64().unwrap(),
                )
            })
            .collect::<std::collections::BTreeSet<_>>()
    };
    let started = identities(&telemetry, "start");
    let summaries = identities(&async_events, "async_summary");
    assert!(started.len() >= 2, "fixture should create a worker isolate");
    assert_eq!(summaries, started);

    let mut removed = false;
    let damaged = async_events
        .lines()
        .filter(|line| {
            let value: Value = serde_json::from_str(line).unwrap();
            if !removed && value["event"] == "async_summary" {
                removed = true;
                false
            } else {
                true
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&async_path, damaged).unwrap();
    cargo_bin_cmd!("v8scope")
        .arg("analyze")
        .arg(&run)
        .arg("--no-report")
        .assert()
        .code(70);
    let manifest = json(&run.join("manifest.json"));
    assert_eq!(manifest["completeness"]["asynchronous"], false);
    assert_eq!(manifest["completeness"]["partial"], true);
}

#[test]
fn readiness_and_rust_load_driver_complete_a_profiled_run() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let ready_url = format!("http://127.0.0.1:{port}/health");
    let load_url = format!("http://127.0.0.1:{port}/items");
    let output = TempDir::new().unwrap();
    cargo_bin_cmd!("v8scope")
        .args(["diagnose", "--output"])
        .arg(output.path())
        .args([
            "--ready-url",
            &ready_url,
            "--load-url",
            &load_url,
            "--connections",
            "2",
            "--load-duration",
            "1s",
            "--",
            "node",
        ])
        .arg(fixture("server.cjs"))
        .arg(port.to_string())
        .assert()
        .success();

    let run = only_run(output.path());
    assert!(run.join("load-summary.json").is_file());
    assert_eq!(
        json(&run.join("manifest.json"))["completeness"]["cpu"],
        true
    );
}

struct Target(Child);

impl Drop for Target {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn attach_profiles_a_real_node_inspector() {
    let mut target = Target(
        Command::new("node")
            .arg("--inspect=127.0.0.1:0")
            .arg(fixture("inspector.cjs"))
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("Node.js is required for the integration suite"),
    );
    let stderr = target.0.stderr.take().unwrap();
    let mut websocket = None;
    for line in BufReader::new(stderr).lines().take(20) {
        let line = line.unwrap();
        if let Some(start) = line.find("ws://") {
            websocket = Some(line[start..].trim().to_string());
            break;
        }
    }
    let websocket = websocket.expect("Node Inspector did not print a WebSocket endpoint");
    let websocket_url = url::Url::parse(&websocket).unwrap();
    let discovery = format!(
        "http://{}:{}",
        websocket_url.host_str().unwrap(),
        websocket_url.port().unwrap()
    );
    let output = TempDir::new().unwrap();
    cargo_bin_cmd!("v8scope")
        .args([
            "attach",
            "--url",
            &discovery,
            "--mode",
            "all",
            "--duration",
            "300ms",
            "--heap-snapshot",
            "--output",
        ])
        .arg(output.path())
        .assert()
        .success();

    let run = only_run(output.path());
    let manifest = json(&run.join("manifest.json"));
    let summary = json(&run.join("summary.json"));
    assert!(
        manifest["runtime"]["node"]
            .as_str()
            .unwrap()
            .starts_with('v')
    );
    assert_eq!(manifest["platform"]["os"], std::env::consts::OS);
    assert_eq!(manifest["platform"]["arch"], std::env::consts::ARCH);
    assert_eq!(manifest["completeness"]["cpu"], true);
    assert_eq!(manifest["completeness"]["heap"], true);
    assert!(summary["cpu"]["profile_samples"].as_u64().unwrap() > 0);
    let snapshot = std::fs::read_dir(run.join("profiles/heap"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().and_then(|value| value.to_str()) == Some("heapsnapshot"))
        .expect("attach should stream a heap snapshot");
    assert!(snapshot.metadata().unwrap().len() > 1_000_000);

    let cpu_output = TempDir::new().unwrap();
    cargo_bin_cmd!("v8scope")
        .args([
            "attach",
            "--url",
            &discovery,
            "--mode",
            "cpu",
            "--duration",
            "150ms",
            "--output",
        ])
        .arg(cpu_output.path())
        .assert()
        .success();
    let cpu_manifest = json(&only_run(cpu_output.path()).join("manifest.json"));
    assert_eq!(cpu_manifest["collectors"]["cpu"], true);
    assert_eq!(cpu_manifest["collectors"]["heap"], false);
    assert_eq!(cpu_manifest["completeness"]["partial"], false);
}

#[test]
fn rejects_non_node_targets() {
    cargo_bin_cmd!("v8scope")
        .args(["diagnose", "--", "echo", "hello"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "target must be a Node.js executable",
        ));
}

#[test]
fn compare_enforces_ci_budgets_and_writes_machine_output() {
    let root = TempDir::new().unwrap();
    let baseline_dir = root.path().join("baseline");
    let candidate_dir = root.path().join("candidate");
    let mut baseline = v8scope::contract::Summary::default();
    baseline.event_loop.delay_p99_ms = 10.0;
    let mut candidate = baseline.clone();
    candidate.event_loop.delay_p99_ms = 12.0;
    write_synthetic_run(&baseline_dir, &baseline);
    write_synthetic_run(&candidate_dir, &candidate);
    let config = root.path().join("v8scope.toml");
    std::fs::write(
        &config,
        "[budgets.event_loop_p99_ms]\nmax = 11\nregression_percent = 10\n",
    )
    .unwrap();

    cargo_bin_cmd!("v8scope")
        .arg("compare")
        .arg(&baseline_dir)
        .arg(&candidate_dir)
        .arg("--config")
        .arg(&config)
        .arg("--json")
        .assert()
        .code(10)
        .stdout(predicates::str::contains("regression_percent"));
    let comparison = json(&candidate_dir.join("comparison.json"));
    assert_eq!(comparison["comparable"], true);
    assert_eq!(comparison["violations"].as_array().unwrap().len(), 2);
    assert_manifest_integrity(&candidate_dir);
}

#[test]
fn compare_rejects_unknown_budget_metrics() {
    let root = TempDir::new().unwrap();
    let baseline_dir = root.path().join("baseline");
    let candidate_dir = root.path().join("candidate");
    write_synthetic_run(&baseline_dir, &v8scope::contract::Summary::default());
    write_synthetic_run(&candidate_dir, &v8scope::contract::Summary::default());
    let config = root.path().join("v8scope.toml");
    std::fs::write(&config, "[budgets.typo_metric]\nmax = -1\n").unwrap();
    cargo_bin_cmd!("v8scope")
        .arg("compare")
        .arg(&baseline_dir)
        .arg(&candidate_dir)
        .arg("--config")
        .arg(&config)
        .assert()
        .code(70)
        .stderr(predicates::str::contains("unknown budget metric"));
}

#[test]
fn compare_rejects_missing_budget_config() {
    let root = TempDir::new().unwrap();
    let baseline_dir = root.path().join("baseline");
    let candidate_dir = root.path().join("candidate");
    write_synthetic_run(&baseline_dir, &v8scope::contract::Summary::default());
    write_synthetic_run(&candidate_dir, &v8scope::contract::Summary::default());
    let missing = root.path().join("missing.toml");

    cargo_bin_cmd!("v8scope")
        .arg("compare")
        .arg(&baseline_dir)
        .arg(&candidate_dir)
        .arg("--config")
        .arg(&missing)
        .assert()
        .code(70)
        .stderr(predicates::str::contains("failed to read budget config"));
}

#[test]
fn compare_rejects_missing_runtime_identity() {
    let root = TempDir::new().unwrap();
    let baseline_dir = root.path().join("baseline");
    let candidate_dir = root.path().join("candidate");
    write_synthetic_run(&baseline_dir, &v8scope::contract::Summary::default());
    write_synthetic_run(&candidate_dir, &v8scope::contract::Summary::default());

    for run in [&baseline_dir, &candidate_dir] {
        let mut manifest: v8scope::contract::Manifest =
            serde_json::from_reader(File::open(run.join("manifest.json")).unwrap()).unwrap();
        manifest.runtime = v8scope::contract::RuntimeInfo::default();
        manifest.platform = v8scope::contract::PlatformInfo::default();
        v8scope::util::atomic_write_json(&run.join("manifest.json"), &manifest).unwrap();
        cargo_bin_cmd!("v8scope")
            .arg("analyze")
            .arg(run)
            .arg("--no-report")
            .assert()
            .success();
    }

    let config = root.path().join("v8scope.toml");
    std::fs::write(&config, "[budgets.event_loop_p99_ms]\nmax = 1\n").unwrap();
    cargo_bin_cmd!("v8scope")
        .arg("compare")
        .arg(&baseline_dir)
        .arg(&candidate_dir)
        .arg("--config")
        .arg(&config)
        .arg("--json")
        .assert()
        .code(70)
        .stdout(predicates::str::contains("Node major is missing"))
        .stdout(predicates::str::contains("V8 major is missing"))
        .stdout(predicates::str::contains("OS is missing"))
        .stdout(predicates::str::contains("architecture is missing"));
    assert_manifest_integrity(&candidate_dir);
}

#[test]
fn schema_command_emits_all_public_contracts() {
    let root = TempDir::new().unwrap();
    let schema = root.path().join("schema.json");
    cargo_bin_cmd!("v8scope")
        .args(["schema", "--output"])
        .arg(&schema)
        .assert()
        .success();
    let schema = json(&schema);
    for contract in ["manifest", "summary", "comparison"] {
        assert!(schema[contract].is_object(), "{contract}");
    }
}
