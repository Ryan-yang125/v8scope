use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::Utc;
use serde::Deserialize;
use serde_json::Value;

use crate::contract::{
    AsyncCallback, AsyncSummary, Comparability, CpuSummary, EventLoopSummary, Finding, GcSummary,
    Hotspot, Manifest, MemorySummary, ResourceSummary, Severity, Summary, manifest_path,
    summary_path,
};
use crate::doctor::{self, CpuAssessment};
use crate::{SCHEMA_VERSION, report, util};

#[derive(Debug, Default)]
pub(crate) struct TelemetryIntegrity {
    pub complete: bool,
    pub warning: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CpuProfile {
    #[serde(default)]
    nodes: Vec<CpuNode>,
    #[serde(default)]
    start_time: f64,
    #[serde(default)]
    end_time: f64,
    #[serde(default)]
    samples: Vec<u64>,
    #[serde(default)]
    time_deltas: Vec<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CpuNode {
    id: u64,
    call_frame: CallFrame,
    #[serde(default)]
    children: Vec<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CallFrame {
    #[serde(default)]
    function_name: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    line_number: i64,
    #[serde(default)]
    column_number: i64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HeapProfile {
    head: HeapNode,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HeapNode {
    call_frame: CallFrame,
    #[serde(default)]
    self_size: f64,
    #[serde(default)]
    children: Vec<HeapNode>,
}

#[derive(Debug, Default, Deserialize)]
struct ExternalProcessSample {
    timestamp_ms: u64,
    cpu_percent: f64,
    rss_bytes: u64,
}

#[derive(Default)]
struct TelemetryAnalysis {
    duration_ms: f64,
    event_loop: EventLoopSummary,
    cpu_samples: Vec<f64>,
    memory: MemorySummary,
    gc: GcSummary,
    resources: ResourceSummary,
}

#[derive(Default)]
struct IsolateTelemetry {
    cpu: Vec<f64>,
    elu: Vec<f64>,
    delay50: Vec<f64>,
    delay95: Vec<f64>,
    delay99: Vec<f64>,
    delay_max: f64,
    rss: Vec<u64>,
    heap: Vec<u64>,
    first_resources: Option<BTreeMap<String, u64>>,
    last_resources: BTreeMap<String, u64>,
    peak_resources: BTreeMap<String, u64>,
}

pub async fn reanalyze(run_dir: &Path, with_report: bool) -> anyhow::Result<u8> {
    let manifest: Manifest = serde_json::from_reader(
        File::open(manifest_path(run_dir))
            .with_context(|| format!("missing manifest in {}", run_dir.display()))?,
    )?;
    let telemetry = analyze_telemetry(&run_dir.join("telemetry.ndjson"))?;
    let external = analyze_process_samples(&run_dir.join("process.ndjson"))?;
    let mut cpu = analyze_cpu_profiles(&run_dir.join("profiles/cpu"), manifest.redact_paths)?;
    let allocation_hotspots =
        analyze_heap_profiles(&run_dir.join("profiles/heap"), manifest.redact_paths)?;
    let asynchronous = analyze_async(
        &run_dir.join("profiles/async/events.ndjson"),
        manifest.redact_paths,
    )?;

    let cpu_observations = if !external.cpu.is_empty() {
        cpu.process_cpu_avg_percent = average(&external.cpu);
        cpu.process_cpu_max_percent = external.cpu.iter().copied().fold(0.0, f64::max);
        external
            .cpu
            .iter()
            .map(|value| value / 100.0)
            .collect::<Vec<_>>()
    } else {
        cpu.process_cpu_avg_percent = average(&telemetry.cpu_samples);
        cpu.process_cpu_max_percent = telemetry.cpu_samples.iter().copied().fold(0.0, f64::max);
        telemetry
            .cpu_samples
            .iter()
            .map(|value| value / 100.0)
            .collect::<Vec<_>>()
    };
    let cpu_assessment = doctor::assess_cpu(&cpu_observations);

    let mut memory = telemetry.memory;
    if !external.rss.is_empty() {
        memory.rss_start_bytes = external.rss.first().copied().unwrap_or_default();
        memory.rss_end_bytes = external.rss.last().copied().unwrap_or_default();
        memory.rss_max_bytes = external.rss.iter().copied().max().unwrap_or_default();
    }
    memory.allocation_hotspots = allocation_hotspots;

    let mut summary = Summary {
        schema_version: SCHEMA_VERSION,
        run_id: manifest.run_id.clone(),
        generated_at: Utc::now(),
        duration_ms: telemetry.duration_ms.max(cpu.profile_duration_ms),
        event_loop: telemetry.event_loop,
        cpu,
        memory,
        gc: telemetry.gc,
        resources: telemetry.resources,
        asynchronous,
        findings: Vec::new(),
        comparability: Comparability {
            node_major: major(manifest.runtime.node.as_deref()),
            v8_major: major(manifest.runtime.v8.as_deref()),
            os: manifest.platform.os.clone(),
            arch: manifest.platform.arch.clone(),
        },
    };
    summary.findings = findings(&summary, cpu_assessment);
    util::atomic_write_json(&summary_path(run_dir), &summary)?;
    if with_report {
        render_cpu_flamegraph(
            &run_dir.join("profiles/cpu"),
            &run_dir.join("report/assets/cpu-flamegraph.svg"),
        )?;
        report::generate(run_dir, &manifest, &summary)?;
    }
    Ok(0)
}

pub async fn execute(run_dir: &Path, with_report: bool) -> anyhow::Result<u8> {
    reanalyze(run_dir, with_report).await?;
    let mut manifest: Manifest = serde_json::from_reader(File::open(manifest_path(run_dir))?)?;
    crate::run::finalize_manifest(run_dir, &mut manifest)?;
    Ok(if manifest.completeness.partial { 70 } else { 0 })
}

pub(crate) fn telemetry_integrity(path: &Path) -> anyhow::Result<TelemetryIntegrity> {
    if !path.is_file() {
        return Ok(TelemetryIntegrity {
            complete: false,
            warning: Some("telemetry is missing".into()),
        });
    }
    let (values, truncated) = read_ndjson_checked(path)?;
    let mut started = HashSet::new();
    let mut finished = HashSet::new();
    for value in values {
        let identity = (integer(&value, "pid"), integer(&value, "thread_id"));
        match value.get("event").and_then(Value::as_str) {
            Some("start") => {
                started.insert(identity);
            }
            Some("finish") => {
                finished.insert(identity);
            }
            _ => {}
        }
    }
    let complete = !started.is_empty() && started == finished && !truncated;
    Ok(TelemetryIntegrity {
        complete,
        warning: (!complete).then(|| {
            format!(
                "telemetry is incomplete: {} isolate(s) started, {} finished{}",
                started.len(),
                finished.len(),
                if truncated {
                    ", final record truncated"
                } else {
                    ""
                }
            )
        }),
    })
}

pub(crate) fn async_integrity(
    path: &Path,
    telemetry_path: &Path,
) -> anyhow::Result<TelemetryIntegrity> {
    if !path.is_file() {
        return Ok(TelemetryIntegrity {
            complete: false,
            warning: Some("async event stream is missing".into()),
        });
    }
    if !telemetry_path.is_file() {
        return Ok(TelemetryIntegrity {
            complete: false,
            warning: Some("async event stream has no telemetry isolate inventory".into()),
        });
    }
    let (values, truncated) = read_ndjson_checked(path)?;
    let (telemetry, _) = read_ndjson_checked(telemetry_path)?;
    let expected = telemetry
        .iter()
        .filter(|value| value.get("event").and_then(Value::as_str) == Some("start"))
        .map(|value| (integer(value, "pid"), integer(value, "thread_id")))
        .collect::<HashSet<_>>();
    let summaries = values
        .iter()
        .filter(|value| value.get("event").and_then(Value::as_str) == Some("async_summary"))
        .map(|value| (integer(value, "pid"), integer(value, "thread_id")))
        .collect::<HashSet<_>>();
    let complete = !expected.is_empty() && summaries == expected && !truncated;
    Ok(TelemetryIntegrity {
        complete,
        warning: (!complete).then(|| {
            format!(
                "async event stream is incomplete: {} of {} isolate(s) emitted a summary{}",
                summaries.intersection(&expected).count(),
                expected.len(),
                if truncated {
                    ", final record truncated"
                } else {
                    ""
                }
            )
        }),
    })
}

fn analyze_telemetry(path: &Path) -> anyhow::Result<TelemetryAnalysis> {
    if !path.is_file() {
        return Ok(TelemetryAnalysis::default());
    }
    let mut analysis = TelemetryAnalysis::default();
    let mut isolates: BTreeMap<(u64, u64), IsolateTelemetry> = BTreeMap::new();
    let mut gc_windows: BTreeMap<u64, f64> = BTreeMap::new();

    for value in read_ndjson(path)? {
        analysis.duration_ms = analysis
            .duration_ms
            .max(number(&value, "timestamp_ns") / 1_000_000.0);
        let identity = (integer(&value, "pid"), integer(&value, "thread_id"));
        match value.get("event").and_then(Value::as_str) {
            Some("sample") => {
                let isolate = isolates.entry(identity).or_default();
                isolate.cpu.push(number(&value, "cpu_percent"));
                isolate.elu.push(number(&value, "event_loop_utilization"));
                isolate
                    .delay50
                    .push(number(&value, "delay_p50_ns") / 1_000_000.0);
                isolate
                    .delay95
                    .push(number(&value, "delay_p95_ns") / 1_000_000.0);
                isolate
                    .delay99
                    .push(number(&value, "delay_p99_ns") / 1_000_000.0);
                isolate.delay_max = isolate
                    .delay_max
                    .max(number(&value, "delay_max_ns") / 1_000_000.0);
                isolate.rss.push(integer(&value, "rss_bytes"));
                isolate.heap.push(integer(&value, "heap_used_bytes"));
                let current = value
                    .get("active_resources")
                    .and_then(Value::as_object)
                    .map(|object| {
                        object
                            .iter()
                            .map(|(key, value)| (key.clone(), value.as_u64().unwrap_or_default()))
                            .collect::<BTreeMap<_, _>>()
                    })
                    .unwrap_or_default();
                isolate
                    .first_resources
                    .get_or_insert_with(|| current.clone());
                isolate.last_resources = current.clone();
                for (kind, count) in current {
                    isolate
                        .peak_resources
                        .entry(kind)
                        .and_modify(|peak: &mut u64| *peak = (*peak).max(count))
                        .or_insert(count);
                }
            }
            Some("gc") => {
                let duration = number(&value, "duration_ms");
                analysis.gc.count += 1;
                analysis.gc.total_pause_ms += duration;
                analysis.gc.max_pause_ms = analysis.gc.max_pause_ms.max(duration);
                let kind = gc_kind(value.get("kind").and_then(Value::as_u64));
                *analysis.gc.by_kind.entry(kind.into()).or_default() += 1;
                let window = integer(&value, "timestamp_ns") / 1_000_000_000;
                *gc_windows.entry(window).or_default() += duration;
            }
            _ => {}
        }
    }

    let mut cpu = Vec::new();
    let mut elu = Vec::new();
    let mut delay50 = Vec::new();
    let mut delay95 = Vec::new();
    let mut delay99 = Vec::new();
    let mut delay_max: f64 = 0.0;
    let mut memory = MemorySummary::default();
    let mut final_resources = BTreeMap::new();
    let mut peak_resources = BTreeMap::new();
    let mut growth = BTreeMap::new();
    for isolate in isolates.values() {
        cpu.extend_from_slice(&isolate.cpu);
        elu.extend_from_slice(&isolate.elu);
        delay50.extend_from_slice(&isolate.delay50);
        delay95.extend_from_slice(&isolate.delay95);
        delay99.extend_from_slice(&isolate.delay99);
        delay_max = delay_max.max(isolate.delay_max);
        memory.rss_start_bytes = memory
            .rss_start_bytes
            .saturating_add(isolate.rss.first().copied().unwrap_or_default());
        memory.rss_end_bytes = memory
            .rss_end_bytes
            .saturating_add(isolate.rss.last().copied().unwrap_or_default());
        memory.rss_max_bytes = memory
            .rss_max_bytes
            .saturating_add(isolate.rss.iter().copied().max().unwrap_or_default());
        memory.heap_used_start_bytes = memory
            .heap_used_start_bytes
            .saturating_add(isolate.heap.first().copied().unwrap_or_default());
        memory.heap_used_end_bytes = memory
            .heap_used_end_bytes
            .saturating_add(isolate.heap.last().copied().unwrap_or_default());
        memory.heap_used_max_bytes = memory
            .heap_used_max_bytes
            .saturating_add(isolate.heap.iter().copied().max().unwrap_or_default());
        let first = isolate
            .first_resources
            .as_ref()
            .cloned()
            .unwrap_or_default();
        for (kind, count) in &isolate.last_resources {
            *final_resources.entry(kind.clone()).or_default() += *count;
        }
        for (kind, count) in &isolate.peak_resources {
            *peak_resources.entry(kind.clone()).or_default() += *count;
        }
        let kinds = first
            .keys()
            .chain(isolate.last_resources.keys())
            .cloned()
            .collect::<HashSet<_>>();
        for kind in kinds {
            *growth.entry(kind.clone()).or_default() += isolate
                .last_resources
                .get(&kind)
                .copied()
                .unwrap_or_default() as i64
                - first.get(&kind).copied().unwrap_or_default() as i64;
        }
    }
    analysis.cpu_samples = cpu;
    let mut elu_for_median = elu.clone();
    analysis.event_loop = EventLoopSummary {
        samples: elu.len() as u64,
        utilization_avg: average(&elu),
        utilization_p50: percentile(&mut elu_for_median, 0.50),
        utilization_max: elu.iter().copied().fold(0.0, f64::max),
        delay_p50_ms: percentile(&mut delay50, 0.50),
        delay_p95_ms: percentile(&mut delay95, 0.95),
        delay_p99_ms: percentile(&mut delay99, 0.99),
        delay_max_ms: delay_max,
    };
    analysis.memory = memory;
    analysis.resources = ResourceSummary {
        final_counts: final_resources,
        peak_counts: peak_resources,
        growth,
    };
    analysis.gc.max_blocking_ms_per_second = gc_windows.values().copied().fold(0.0, f64::max);
    Ok(analysis)
}

#[derive(Default)]
struct ExternalAnalysis {
    cpu: Vec<f64>,
    rss: Vec<u64>,
}

fn analyze_process_samples(path: &Path) -> anyhow::Result<ExternalAnalysis> {
    if !path.is_file() {
        return Ok(ExternalAnalysis::default());
    }
    let mut groups: BTreeMap<u64, (f64, u64)> = BTreeMap::new();
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let sample: ExternalProcessSample = match serde_json::from_str(&line) {
            Ok(sample) => sample,
            Err(_) => continue,
        };
        let group = groups.entry(sample.timestamp_ms).or_default();
        group.0 += sample.cpu_percent;
        group.1 = group.1.saturating_add(sample.rss_bytes);
    }
    Ok(ExternalAnalysis {
        cpu: groups.values().map(|value| value.0).collect(),
        rss: groups.values().map(|value| value.1).collect(),
    })
}

fn analyze_cpu_profiles(directory: &Path, redact_paths: bool) -> anyhow::Result<CpuSummary> {
    let mut summary = CpuSummary::default();
    let mut aggregate: HashMap<(String, String, i64), (f64, f64)> = HashMap::new();
    for path in profile_files(directory, "cpuprofile")? {
        let profile: CpuProfile = serde_json::from_reader(File::open(&path)?)
            .with_context(|| format!("invalid CPU profile {}", path.display()))?;
        summary.profile_duration_ms += (profile.end_time - profile.start_time).max(0.0) / 1000.0;
        summary.profile_samples += profile.samples.len() as u64;
        let mut parents = HashMap::new();
        for node in &profile.nodes {
            for child in &node.children {
                parents.insert(*child, node.id);
            }
        }
        let frames = profile
            .nodes
            .iter()
            .map(|node| (node.id, mapped_frame(&node.call_frame, redact_paths)))
            .collect::<HashMap<_, _>>();
        for (index, sample) in profile.samples.iter().enumerate() {
            let delta = profile.time_deltas.get(index).copied().unwrap_or(1000.0) / 1000.0;
            if let Some(frame) = frames.get(sample) {
                aggregate.entry(frame.clone()).or_default().0 += delta;
            }
            let mut current = Some(*sample);
            let mut seen_nodes = HashSet::new();
            let mut seen_frames = HashSet::new();
            while let Some(id) = current {
                if !seen_nodes.insert(id) {
                    break;
                }
                if let Some(frame) = frames.get(&id)
                    && seen_frames.insert(frame.clone())
                {
                    aggregate.entry(frame.clone()).or_default().1 += delta;
                }
                current = parents.get(&id).copied();
            }
        }
    }
    summary.hotspots = aggregate
        .into_iter()
        .filter(|(_, values)| values.1 > 0.0)
        .map(
            |((function, url, line), (self_value, total_value))| Hotspot {
                function,
                url,
                line,
                self_value,
                total_value,
                unit: "ms".into(),
            },
        )
        .collect();
    summary.hotspots.sort_by(|left, right| {
        right
            .total_value
            .total_cmp(&left.total_value)
            .then_with(|| right.self_value.total_cmp(&left.self_value))
    });
    summary.hotspots.truncate(50);
    Ok(summary)
}

fn render_cpu_flamegraph(directory: &Path, output: &Path) -> anyhow::Result<()> {
    let mut folded: BTreeMap<String, u64> = BTreeMap::new();
    for path in profile_files(directory, "cpuprofile")? {
        let profile: CpuProfile = match serde_json::from_reader(File::open(&path)?) {
            Ok(profile) => profile,
            Err(_) => continue,
        };
        let nodes: HashMap<_, _> = profile.nodes.iter().map(|node| (node.id, node)).collect();
        let mut parents = HashMap::new();
        for node in &profile.nodes {
            for child in &node.children {
                parents.insert(*child, node.id);
            }
        }
        for (index, sample) in profile.samples.iter().enumerate() {
            let mut stack = Vec::new();
            let mut current = Some(*sample);
            let mut seen = HashSet::new();
            while let Some(id) = current {
                if !seen.insert(id) {
                    break;
                }
                if let Some(node) = nodes.get(&id) {
                    stack.push(flame_label(&node.call_frame));
                }
                current = parents.get(&id).copied();
            }
            stack.reverse();
            if stack.is_empty() {
                continue;
            }
            let weight = profile
                .time_deltas
                .get(index)
                .copied()
                .unwrap_or(1000.0)
                .max(1.0)
                .round() as u64;
            *folded.entry(stack.join(";")).or_default() += weight;
        }
    }
    if folded.is_empty() {
        if output.is_file() {
            std::fs::remove_file(output)?;
        }
        return Ok(());
    }
    let input = folded
        .into_iter()
        .map(|(stack, weight)| format!("{stack} {weight}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut svg = Vec::new();
    let mut options = inferno::flamegraph::Options::default();
    options.title = "V8Scope CPU flame graph".into();
    options.count_name = "microseconds".into();
    options.deterministic = true;
    inferno::flamegraph::from_reader(&mut options, input.as_bytes(), &mut svg)?;
    util::atomic_write(output, &svg)?;
    Ok(())
}

fn flame_label(frame: &CallFrame) -> String {
    let name = if frame.function_name.is_empty() {
        "(anonymous)"
    } else {
        &frame.function_name
    };
    let location = local_script_path(&frame.url)
        .and_then(|path| {
            path.file_name()
                .map(|value| value.to_string_lossy().into_owned())
        })
        .unwrap_or_default();
    let label = if location.is_empty() {
        name.to_string()
    } else {
        format!("{name} ({location}:{})", frame.line_number + 1)
    };
    label.replace([';', '\n', '\r'], ":")
}

fn analyze_heap_profiles(directory: &Path, redact_paths: bool) -> anyhow::Result<Vec<Hotspot>> {
    let mut aggregate: HashMap<(String, String, i64), (f64, f64)> = HashMap::new();
    for path in profile_files(directory, "heapprofile")? {
        let profile: HeapProfile = serde_json::from_reader(File::open(&path)?)
            .with_context(|| format!("invalid heap profile {}", path.display()))?;
        aggregate_heap(&profile.head, redact_paths, &mut aggregate);
    }
    let mut hotspots: Vec<_> = aggregate
        .into_iter()
        .filter(|(_, values)| values.0 > 0.0)
        .map(
            |((function, url, line), (self_value, total_value))| Hotspot {
                function,
                url,
                line,
                self_value,
                total_value,
                unit: "bytes".into(),
            },
        )
        .collect();
    hotspots.sort_by(|left, right| right.self_value.total_cmp(&left.self_value));
    hotspots.truncate(50);
    Ok(hotspots)
}

fn aggregate_heap(
    node: &HeapNode,
    redact_paths: bool,
    aggregate: &mut HashMap<(String, String, i64), (f64, f64)>,
) -> f64 {
    let child_total = node
        .children
        .iter()
        .map(|child| aggregate_heap(child, redact_paths, aggregate))
        .sum::<f64>();
    let total = node.self_size + child_total;
    let key = mapped_frame(&node.call_frame, redact_paths);
    let entry = aggregate.entry(key).or_default();
    entry.0 += node.self_size;
    entry.1 += total;
    total
}

fn mapped_frame(frame: &CallFrame, redact_paths: bool) -> (String, String, i64) {
    let mut name = if frame.function_name.is_empty() {
        "(anonymous)".to_string()
    } else {
        frame.function_name.clone()
    };
    let mut url = frame.url.clone();
    let mut line = frame.line_number + 1;
    let path = local_script_path(&url);
    if let Some(path) = path {
        let map_path = PathBuf::from(format!("{}.map", path.display()));
        if let Ok(bytes) = std::fs::read(&map_path)
            && let Ok(map) = sourcemap::decode_slice(&bytes)
            && let Some(token) = map.lookup_token(
                frame.line_number.max(0) as u32,
                frame.column_number.max(0) as u32,
            )
        {
            if let Some(source) = token.get_source() {
                url = source.to_string();
            }
            line = token.get_src_line() as i64 + 1;
            if let Some(token_name) = token.get_name() {
                name = token_name.to_string();
            }
        }
    }
    if redact_paths {
        url = redact_location(&url);
    }
    (name, url, line)
}

fn local_script_path(url: &str) -> Option<PathBuf> {
    if url.starts_with("file://") {
        return url::Url::parse(url).ok()?.to_file_path().ok();
    }
    let path = PathBuf::from(url);
    path.is_absolute().then_some(path)
}

#[derive(Clone)]
struct AsyncResourceData {
    kind: String,
    trigger: u64,
    stack: Vec<String>,
    initialized_ns: u64,
}

fn analyze_async(path: &Path, redact_paths: bool) -> anyhow::Result<AsyncSummary> {
    if !path.is_file() {
        return Ok(AsyncSummary::default());
    }
    let mut summary = AsyncSummary {
        enabled: true,
        ..Default::default()
    };
    let mut live = HashSet::new();
    let mut resources: HashMap<(u64, u64, u64), AsyncResourceData> = HashMap::new();
    let mut waits_by_type: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut saw_summary = false;
    for value in read_ndjson(path)? {
        match value.get("event").and_then(Value::as_str) {
            Some("init") => {
                summary.events += 1;
                let id = integer(&value, "async_id");
                let pid = integer(&value, "pid");
                let thread_id = integer(&value, "thread_id");
                let kind = value
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("Unknown")
                    .to_string();
                let trigger = integer(&value, "trigger_async_id");
                let stack = value
                    .get("stack")
                    .and_then(Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::trim)
                            .map(|line| redact_path(line, redact_paths))
                            .collect()
                    })
                    .unwrap_or_default();
                resources.insert(
                    (pid, thread_id, id),
                    AsyncResourceData {
                        kind: kind.clone(),
                        trigger,
                        stack,
                        initialized_ns: integer(&value, "timestamp_ns"),
                    },
                );
                live.insert((pid, thread_id, id));
                *summary.by_type.entry(kind).or_default() += 1;
            }
            Some("destroy") => {
                summary.events += 1;
                live.remove(&(
                    integer(&value, "pid"),
                    integer(&value, "thread_id"),
                    integer(&value, "async_id"),
                ));
            }
            Some("callback") => {
                summary.events += 1;
                let id = integer(&value, "async_id");
                let pid = integer(&value, "pid");
                let thread_id = integer(&value, "thread_id");
                let duration_ms = number(&value, "duration_ns") / 1_000_000.0;
                let key = (pid, thread_id, id);
                let resource = resources.get(&key);
                let kind = resource
                    .map(|resource| resource.kind.clone())
                    .unwrap_or_else(|| "Unknown".into());
                let timestamp_ns = integer(&value, "timestamp_ns");
                let wait_ms = value
                    .get("wait_ns")
                    .and_then(Value::as_f64)
                    .unwrap_or_else(|| {
                        resource
                            .map(|resource| {
                                timestamp_ns
                                    .saturating_sub(resource.initialized_ns)
                                    .saturating_sub(number(&value, "duration_ns") as u64)
                                    as f64
                            })
                            .unwrap_or_default()
                    })
                    / 1_000_000.0;
                let lifetime_ms = resource
                    .map(|resource| {
                        timestamp_ns.saturating_sub(resource.initialized_ns) as f64 / 1_000_000.0
                    })
                    .unwrap_or_default();
                *summary
                    .callback_time_ms_by_type
                    .entry(kind.clone())
                    .or_default() += duration_ms;
                *summary
                    .wait_time_ms_by_type
                    .entry(kind.clone())
                    .or_default() += wait_ms;
                waits_by_type.entry(kind.clone()).or_default().push(wait_ms);
                let topology = summary.topology.entry(kind.clone()).or_default();
                topology.callbacks += 1;
                topology.total_callback_ms += duration_ms;
                topology.total_wait_ms += wait_ms;
                if let Some(resource) = resource
                    && let Some(parent) = resources.get(&(pid, thread_id, resource.trigger))
                {
                    *summary
                        .causal_edges
                        .entry(format!("{} -> {}", parent.kind, resource.kind))
                        .or_default() += 1;
                }
                summary.slow_callbacks.push(AsyncCallback {
                    pid,
                    thread_id,
                    async_id: id,
                    resource_type: kind,
                    duration_ms,
                    wait_ms,
                    lifetime_ms,
                    stack: resource
                        .map(|value| value.stack.clone())
                        .unwrap_or_default(),
                    causal_chain: causal_chain(key, &resources),
                });
            }
            Some("async_summary") => {
                saw_summary = true;
                summary.dropped += integer(&value, "dropped");
                summary.live_resources += integer(&value, "live_resources");
            }
            Some(_) => summary.events += 1,
            None => {}
        }
    }
    if !saw_summary {
        summary.live_resources = live.len() as u64;
    }
    for (kind, count) in &summary.by_type {
        summary.topology.entry(kind.clone()).or_default().resources = *count;
    }
    for (kind, mut waits) in waits_by_type {
        let topology = summary.topology.entry(kind).or_default();
        topology.wait_max_ms = waits.iter().copied().fold(0.0, f64::max);
        topology.wait_p95_ms = percentile(&mut waits, 0.95);
    }
    summary.slow_callbacks.sort_by(|left, right| {
        (right.wait_ms + right.duration_ms).total_cmp(&(left.wait_ms + left.duration_ms))
    });
    summary.slow_callbacks.truncate(50);
    Ok(summary)
}

fn causal_chain(
    start: (u64, u64, u64),
    resources: &HashMap<(u64, u64, u64), AsyncResourceData>,
) -> Vec<String> {
    let mut chain = Vec::new();
    let mut current = Some(start);
    let mut seen = HashSet::new();
    while let Some(id) = current {
        if !seen.insert(id) || chain.len() == 16 {
            break;
        }
        let Some(resource) = resources.get(&id) else {
            break;
        };
        chain.push(resource.kind.clone());
        current = (resource.trigger != 0).then_some((id.0, id.1, resource.trigger));
    }
    chain.reverse();
    chain
}

fn redact_path(value: &str, enabled: bool) -> String {
    if !enabled {
        value.to_string()
    } else {
        value
            .split_whitespace()
            .map(|token| {
                let prefix = token.trim_start_matches('(');
                let suffix = prefix.trim_end_matches(')');
                let location = suffix
                    .rsplit_once(':')
                    .and_then(|(without_column, _)| without_column.rsplit_once(':'))
                    .map(|(path, _)| path)
                    .unwrap_or(suffix);
                if is_absolute_location(location) {
                    token.replace(location, &redact_location(location))
                } else {
                    token.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn redact_location(value: &str) -> String {
    let file_name = local_script_path(value)
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .or_else(|| {
            let normalized = value.replace('\\', "/");
            is_absolute_location(value).then(|| {
                normalized
                    .rsplit('/')
                    .next()
                    .unwrap_or("source")
                    .to_string()
            })
        });
    file_name
        .map(|name| format!("<project>/{name}"))
        .unwrap_or_else(|| value.to_string())
}

fn is_absolute_location(value: &str) -> bool {
    value.starts_with('/')
        || value.starts_with("file://")
        || (value.len() >= 3
            && value.as_bytes()[1] == b':'
            && matches!(value.as_bytes()[2], b'/' | b'\\'))
}

fn findings(summary: &Summary, cpu_assessment: CpuAssessment) -> Vec<Finding> {
    let mut findings = Vec::new();
    if cpu_assessment == CpuAssessment::Performance {
        findings.push(finding(
            "cpu-underutilization",
            Severity::Warning,
            "cpu",
            "Application CPU utilization indicates idle or I/O-bound periods",
            [
                ("average_percent", Value::from(summary.cpu.process_cpu_avg_percent)),
                ("maximum_percent", Value::from(summary.cpu.process_cpu_max_percent)),
            ],
            "Inspect slow external calls and async causality; verify that the workload keeps the application supplied with work.",
        ));
    }
    if summary.event_loop.delay_p50_ms >= 10.0 || summary.event_loop.delay_max_ms >= 100.0 {
        findings.push(finding(
            "event-loop-delay",
            if summary.event_loop.delay_max_ms >= 250.0 { Severity::Critical } else { Severity::Warning },
            "event_loop",
            "Event-loop latency is elevated",
            [
                ("p50_ms", Value::from(summary.event_loop.delay_p50_ms)),
                ("max_ms", Value::from(summary.event_loop.delay_max_ms)),
            ],
            "Inspect the CPU profile around latency spikes and move synchronous or CPU-heavy work off the request path.",
        ));
    }
    if summary.event_loop.utilization_p50 >= 0.95 {
        findings.push(finding(
            "event-loop-saturation",
            Severity::Warning,
            "cpu",
            "The event loop is saturated",
            [("utilization_p50", Value::from(summary.event_loop.utilization_p50))],
            "Start with the highest self-time functions in the CPU profile and measure after each change.",
        ));
    }
    let heap_growth = summary
        .memory
        .heap_used_end_bytes
        .saturating_sub(summary.memory.heap_used_start_bytes);
    if heap_growth >= 16 * 1024 * 1024
        && summary.memory.heap_used_end_bytes as f64
            >= summary.memory.heap_used_start_bytes.max(1) as f64 * 1.25
    {
        findings.push(finding(
            "heap-growth",
            Severity::Warning,
            "memory",
            "Heap usage grew throughout the run",
            [("growth_bytes", Value::from(heap_growth))],
            "Repeat the same workload and compare allocation hotspots; capture heap snapshots when retained growth persists.",
        ));
    }
    if summary.gc.max_blocking_ms_per_second >= 100.0 {
        findings.push(finding(
            "gc-pause",
            Severity::Warning,
            "gc",
            "A long garbage-collection pause was observed",
            [(
                "max_blocking_ms_per_second",
                Value::from(summary.gc.max_blocking_ms_per_second),
            )],
            "Inspect temporary allocation hotspots and reduce allocation volume on latency-sensitive paths.",
        ));
    }
    let growing: BTreeMap<_, _> = summary
        .resources
        .growth
        .iter()
        .filter(|(_, growth)| **growth >= 10)
        .map(|(kind, growth)| (kind.clone(), Value::from(*growth)))
        .collect();
    if !growing.is_empty() {
        findings.push(Finding {
            id: "active-resource-growth".into(),
            severity: Severity::Warning,
            category: "resources".into(),
            title: "Active resources increased during the run".into(),
            evidence: growing,
            recommendation: "Check that timers, sockets, file handles, and requests are closed on every completion and error path.".into(),
        });
    }
    if summary.asynchronous.enabled && summary.asynchronous.dropped > 0 {
        findings.push(finding(
            "async-events-dropped",
            Severity::Info,
            "data_quality",
            "The async event cap was reached",
            [("dropped", Value::from(summary.asynchronous.dropped))],
            "Use a shorter capture window or raise --async-max-events after measuring available memory.",
        ));
    }
    findings
}

fn finding<const N: usize>(
    id: &str,
    severity: Severity,
    category: &str,
    title: &str,
    evidence: [(&str, Value); N],
    recommendation: &str,
) -> Finding {
    Finding {
        id: id.into(),
        severity,
        category: category.into(),
        title: title.into(),
        evidence: evidence
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect(),
        recommendation: recommendation.into(),
    }
}

fn profile_files(directory: &Path, extension: &str) -> anyhow::Result<Vec<PathBuf>> {
    if !directory.is_dir() {
        return Ok(Vec::new());
    }
    let mut files: Vec<_> = std::fs::read_dir(directory)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some(extension))
        .collect();
    files.sort();
    Ok(files)
}

fn read_ndjson(path: &Path) -> anyhow::Result<Vec<Value>> {
    Ok(read_ndjson_checked(path)?.0)
}

fn read_ndjson_checked(path: &Path) -> anyhow::Result<(Vec<Value>, bool)> {
    let mut values = Vec::new();
    let lines = BufReader::new(File::open(path)?)
        .lines()
        .collect::<Result<Vec<_>, _>>()?;
    let last_nonempty = lines.iter().rposition(|line| !line.trim().is_empty());
    let mut truncated = false;
    for (index, line) in lines.into_iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str(&line) {
            Ok(value) => values.push(value),
            Err(error) if Some(index) == last_nonempty => {
                truncated = true;
                let _ = error;
            }
            Err(error) => anyhow::bail!(
                "invalid NDJSON record {} in {}: {error}",
                index + 1,
                path.display()
            ),
        }
    }
    Ok((values, truncated))
}

fn number(value: &Value, key: &str) -> f64 {
    value.get(key).and_then(Value::as_f64).unwrap_or_default()
}

fn integer(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or_default()
}

fn average(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

fn percentile(values: &mut [f64], quantile: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    let rank = quantile.clamp(0.0, 1.0) * (values.len() - 1) as f64;
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    if lower == upper {
        values[lower]
    } else {
        values[lower] + (values[upper] - values[lower]) * (rank - lower as f64)
    }
}

fn gc_kind(kind: Option<u64>) -> &'static str {
    match kind {
        Some(1) => "minor",
        Some(4) => "major",
        Some(8) => "incremental",
        Some(16) => "weak_callbacks",
        _ => "unknown",
    }
}

fn major(version: Option<&str>) -> Option<u32> {
    version?
        .trim_start_matches('v')
        .split('.')
        .next()?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates_percentiles() {
        let mut values = vec![1.0, 2.0, 3.0, 4.0];
        assert_eq!(percentile(&mut values, 0.5), 2.5);
        assert_eq!(percentile(&mut values, 1.0), 4.0);
    }

    #[test]
    fn categorizes_gc_kinds() {
        assert_eq!(gc_kind(Some(1)), "minor");
        assert_eq!(gc_kind(Some(4)), "major");
        assert_eq!(gc_kind(Some(8)), "incremental");
        assert_eq!(gc_kind(Some(16)), "weak_callbacks");
        assert_eq!(gc_kind(None), "unknown");
    }

    #[test]
    fn builds_async_causal_chains_and_ranks_callbacks() {
        use std::io::Write;

        let mut file = tempfile::NamedTempFile::new().unwrap();
        for event in [
            serde_json::json!({"event":"init","pid":10,"thread_id":1,"async_id":1,"trigger_async_id":0,"type":"Timeout","stack":["at timer (app.js:1:1)"]}),
            serde_json::json!({"event":"init","pid":10,"thread_id":1,"async_id":2,"trigger_async_id":1,"type":"PROMISE","stack":["at query (app.js:2:1)"]}),
            serde_json::json!({"event":"callback","pid":10,"thread_id":1,"async_id":2,"duration_ns":5_000_000}),
            serde_json::json!({"event":"async_summary","pid":10,"thread_id":1,"events":3,"dropped":0,"live_resources":2}),
            serde_json::json!({"event":"init","pid":20,"thread_id":1,"async_id":1,"trigger_async_id":0,"type":"Immediate","stack":[]}),
            serde_json::json!({"event":"init","pid":20,"thread_id":1,"async_id":2,"trigger_async_id":1,"type":"TCPWRAP","stack":["at remote (worker.js:1:1)"]}),
            serde_json::json!({"event":"callback","pid":20,"thread_id":1,"async_id":2,"duration_ns":6_000_000}),
            serde_json::json!({"event":"async_summary","pid":20,"thread_id":1,"events":3,"dropped":1,"live_resources":1}),
        ] {
            writeln!(file, "{event}").unwrap();
        }
        let summary = analyze_async(file.path(), false).unwrap();
        assert_eq!(summary.causal_edges["Timeout -> PROMISE"], 1);
        assert_eq!(summary.causal_edges["Immediate -> TCPWRAP"], 1);
        assert_eq!(summary.dropped, 1);
        assert_eq!(summary.live_resources, 3);
        assert_eq!(summary.slow_callbacks[0].duration_ms, 6.0);
        assert_eq!(summary.slow_callbacks[1].duration_ms, 5.0);
        assert_eq!(
            summary.slow_callbacks[1].causal_chain,
            ["Timeout", "PROMISE"]
        );
        assert_eq!(summary.slow_callbacks[1].stack[0], "at query (app.js:2:1)");
    }

    #[test]
    fn measures_async_wait_separately_from_callback_execution() {
        use std::io::Write;

        let mut file = tempfile::NamedTempFile::new().unwrap();
        for event in [
            serde_json::json!({"event":"init","pid":1,"thread_id":0,"timestamp_ns":1_000_000,"async_id":2,"trigger_async_id":0,"type":"TCPWRAP","stack":[]}),
            serde_json::json!({"event":"callback","pid":1,"thread_id":0,"timestamp_ns":5_101_000_000_u64,"async_id":2,"wait_ns":5_000_000_000_u64,"duration_ns":100_000}),
            serde_json::json!({"event":"async_summary","pid":1,"thread_id":0,"events":2,"dropped":0,"live_resources":1}),
        ] {
            writeln!(file, "{event}").unwrap();
        }
        let summary = analyze_async(file.path(), false).unwrap();
        let operation = &summary.slow_callbacks[0];
        assert_eq!(operation.wait_ms, 5_000.0);
        assert_eq!(operation.duration_ms, 0.1);
        assert_eq!(summary.topology["TCPWRAP"].wait_max_ms, 5_000.0);
        assert_eq!(summary.wait_time_ms_by_type["TCPWRAP"], 5_000.0);
    }

    #[test]
    fn aggregates_telemetry_per_isolate_before_combining_memory_and_resources() {
        use std::io::Write;

        let mut file = tempfile::NamedTempFile::new().unwrap();
        for event in [
            serde_json::json!({"event":"start","pid":1,"thread_id":0,"timestamp_ns":0}),
            serde_json::json!({"event":"sample","pid":1,"thread_id":0,"timestamp_ns":1,"rss_bytes":10,"heap_used_bytes":10,"active_resources":{"Timeout":1}}),
            serde_json::json!({"event":"sample","pid":1,"thread_id":0,"timestamp_ns":2,"rss_bytes":20,"heap_used_bytes":20,"active_resources":{"Timeout":2}}),
            serde_json::json!({"event":"finish","pid":1,"thread_id":0,"timestamp_ns":3}),
            serde_json::json!({"event":"start","pid":2,"thread_id":0,"timestamp_ns":0}),
            serde_json::json!({"event":"sample","pid":2,"thread_id":0,"timestamp_ns":1,"rss_bytes":100,"heap_used_bytes":100,"active_resources":{"Timeout":3}}),
            serde_json::json!({"event":"sample","pid":2,"thread_id":0,"timestamp_ns":2,"rss_bytes":80,"heap_used_bytes":80,"active_resources":{"Timeout":1}}),
            serde_json::json!({"event":"finish","pid":2,"thread_id":0,"timestamp_ns":3}),
        ] {
            writeln!(file, "{event}").unwrap();
        }
        let analysis = analyze_telemetry(file.path()).unwrap();
        assert_eq!(analysis.memory.heap_used_start_bytes, 110);
        assert_eq!(analysis.memory.heap_used_end_bytes, 100);
        assert_eq!(analysis.memory.heap_used_max_bytes, 120);
        assert_eq!(analysis.resources.final_counts["Timeout"], 3);
        assert_eq!(analysis.resources.growth["Timeout"], -1);
        assert!(telemetry_integrity(file.path()).unwrap().complete);
    }

    #[test]
    fn recursive_cpu_frames_do_not_double_count_inclusive_time() {
        let root = tempfile::tempdir().unwrap();
        let profile = serde_json::json!({
            "nodes": [
                {"id":1,"callFrame":{"functionName":"recur","url":"/tmp/app.js","lineNumber":0,"columnNumber":0},"children":[2]},
                {"id":2,"callFrame":{"functionName":"recur","url":"/tmp/app.js","lineNumber":0,"columnNumber":0},"children":[]}
            ],
            "startTime":0,
            "endTime":1000,
            "samples":[2],
            "timeDeltas":[1000]
        });
        std::fs::write(
            root.path().join("recursive.cpuprofile"),
            profile.to_string(),
        )
        .unwrap();
        let summary = analyze_cpu_profiles(root.path(), false).unwrap();
        let recur = summary
            .hotspots
            .iter()
            .find(|hotspot| hotspot.function == "recur")
            .unwrap();
        assert_eq!(recur.self_value, 1.0);
        assert_eq!(recur.total_value, 1.0);
        assert!(recur.total_value <= summary.profile_duration_ms);
    }

    #[test]
    fn cpu_hotspots_include_inclusive_only_ancestors() {
        let root = tempfile::tempdir().unwrap();
        let profile = serde_json::json!({
            "nodes": [
                {"id":1,"callFrame":{"functionName":"wrapper","url":"/tmp/app.js","lineNumber":0,"columnNumber":0},"children":[2]},
                {"id":2,"callFrame":{"functionName":"leaf","url":"/tmp/app.js","lineNumber":1,"columnNumber":0},"children":[]}
            ],
            "startTime":0,
            "endTime":1000,
            "samples":[2],
            "timeDeltas":[1000]
        });
        std::fs::write(root.path().join("ancestor.cpuprofile"), profile.to_string()).unwrap();
        let summary = analyze_cpu_profiles(root.path(), false).unwrap();
        let wrapper = summary
            .hotspots
            .iter()
            .find(|hotspot| hotspot.function == "wrapper")
            .expect("inclusive ancestor should be retained");
        assert_eq!(wrapper.self_value, 0.0);
        assert_eq!(wrapper.total_value, 1.0);
    }

    #[test]
    fn async_integrity_requires_a_summary_for_every_telemetry_isolate() {
        let root = tempfile::tempdir().unwrap();
        let telemetry = root.path().join("telemetry.ndjson");
        let asynchronous = root.path().join("async.ndjson");
        std::fs::write(
            &telemetry,
            concat!(
                "{\"event\":\"start\",\"pid\":1,\"thread_id\":0}\n",
                "{\"event\":\"finish\",\"pid\":1,\"thread_id\":0}\n",
                "{\"event\":\"start\",\"pid\":1,\"thread_id\":1}\n",
                "{\"event\":\"finish\",\"pid\":1,\"thread_id\":1}\n"
            ),
        )
        .unwrap();
        std::fs::write(
            &asynchronous,
            "{\"event\":\"async_summary\",\"pid\":1,\"thread_id\":0}\n",
        )
        .unwrap();

        let integrity = async_integrity(&asynchronous, &telemetry).unwrap();
        assert!(!integrity.complete);
        assert!(integrity.warning.unwrap().contains("1 of 2 isolate"));
    }

    #[test]
    fn rejects_malformed_ndjson_before_the_final_record() {
        use std::io::Write;

        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "{{\"event\":\"start\",\"pid\":1,\"thread_id\":0}}").unwrap();
        writeln!(file, "invalid").unwrap();
        writeln!(file, "{{\"event\":\"finish\",\"pid\":1,\"thread_id\":0}}").unwrap();
        assert!(read_ndjson(file.path()).is_err());
    }
}
