use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::{DateTime, Utc};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};

use crate::SCHEMA_VERSION;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Diagnose,
    Cpu,
    Heap,
    Async,
    All,
    Attach,
}

impl Mode {
    pub fn captures_cpu(self) -> bool {
        matches!(self, Self::Diagnose | Self::Cpu | Self::All | Self::Attach)
    }

    pub fn captures_heap(self) -> bool {
        matches!(self, Self::Heap | Self::All | Self::Attach)
    }

    pub fn captures_async(self) -> bool {
        matches!(self, Self::Async | Self::All)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Manifest {
    pub schema_version: u32,
    pub v8scope_version: String,
    pub run_id: String,
    pub name: String,
    pub mode: Mode,
    pub collectors: CollectorSet,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub command: Vec<String>,
    pub cwd: String,
    pub redact_paths: bool,
    pub platform: PlatformInfo,
    pub runtime: RuntimeInfo,
    pub process: ProcessResult,
    pub completeness: Completeness,
    pub files: Vec<Artifact>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct CollectorSet {
    pub telemetry: bool,
    pub cpu: bool,
    pub heap: bool,
    pub asynchronous: bool,
}

impl CollectorSet {
    pub fn launch(mode: Mode) -> Self {
        Self {
            telemetry: true,
            cpu: mode.captures_cpu(),
            heap: mode.captures_heap(),
            asynchronous: mode.captures_async(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct PlatformInfo {
    pub os: String,
    pub arch: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeInfo {
    pub node: Option<String>,
    pub v8: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ProcessResult {
    pub root_pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub interrupted: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Completeness {
    pub telemetry: bool,
    pub cpu: bool,
    pub heap: bool,
    pub asynchronous: bool,
    pub partial: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Artifact {
    pub path: String,
    pub kind: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Summary {
    pub schema_version: u32,
    pub run_id: String,
    pub generated_at: DateTime<Utc>,
    pub duration_ms: f64,
    pub event_loop: EventLoopSummary,
    pub cpu: CpuSummary,
    pub memory: MemorySummary,
    pub gc: GcSummary,
    pub resources: ResourceSummary,
    pub asynchronous: AsyncSummary,
    pub findings: Vec<Finding>,
    pub comparability: Comparability,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct EventLoopSummary {
    pub samples: u64,
    pub utilization_avg: f64,
    pub utilization_p50: f64,
    pub utilization_max: f64,
    pub delay_p50_ms: f64,
    pub delay_p95_ms: f64,
    pub delay_p99_ms: f64,
    pub delay_max_ms: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct CpuSummary {
    pub process_cpu_avg_percent: f64,
    pub process_cpu_max_percent: f64,
    pub profile_duration_ms: f64,
    pub profile_samples: u64,
    pub hotspots: Vec<Hotspot>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct MemorySummary {
    pub rss_start_bytes: u64,
    pub rss_end_bytes: u64,
    pub rss_max_bytes: u64,
    pub heap_used_start_bytes: u64,
    pub heap_used_end_bytes: u64,
    pub heap_used_max_bytes: u64,
    pub allocation_hotspots: Vec<Hotspot>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct GcSummary {
    pub count: u64,
    pub total_pause_ms: f64,
    pub max_pause_ms: f64,
    pub max_blocking_ms_per_second: f64,
    pub by_kind: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ResourceSummary {
    pub final_counts: BTreeMap<String, u64>,
    pub peak_counts: BTreeMap<String, u64>,
    pub growth: BTreeMap<String, i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct AsyncSummary {
    pub enabled: bool,
    pub events: u64,
    pub dropped: u64,
    pub live_resources: u64,
    pub by_type: BTreeMap<String, u64>,
    pub callback_time_ms_by_type: BTreeMap<String, f64>,
    pub wait_time_ms_by_type: BTreeMap<String, f64>,
    pub topology: BTreeMap<String, AsyncTypeSummary>,
    pub causal_edges: BTreeMap<String, u64>,
    pub slow_callbacks: Vec<AsyncCallback>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct AsyncCallback {
    pub pid: u64,
    pub thread_id: u64,
    pub async_id: u64,
    pub resource_type: String,
    pub duration_ms: f64,
    pub wait_ms: f64,
    pub lifetime_ms: f64,
    pub stack: Vec<String>,
    pub causal_chain: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct AsyncTypeSummary {
    pub resources: u64,
    pub callbacks: u64,
    pub total_callback_ms: f64,
    pub total_wait_ms: f64,
    pub wait_p95_ms: f64,
    pub wait_max_ms: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Hotspot {
    pub function: String,
    pub url: String,
    pub line: i64,
    pub self_value: f64,
    pub total_value: f64,
    pub unit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Finding {
    pub id: String,
    pub severity: Severity,
    pub category: String,
    pub title: String,
    pub evidence: BTreeMap<String, serde_json::Value>,
    pub recommendation: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Comparability {
    pub node_major: Option<u32>,
    pub v8_major: Option<u32>,
    pub os: String,
    pub arch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Comparison {
    pub schema_version: u32,
    pub comparable: bool,
    pub reasons: Vec<String>,
    pub metrics: BTreeMap<String, MetricDelta>,
    pub violations: Vec<BudgetViolation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MetricDelta {
    pub baseline: f64,
    pub candidate: f64,
    pub delta: f64,
    pub percent: Option<f64>,
    pub unit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BudgetViolation {
    pub metric: String,
    pub observed: f64,
    pub limit: f64,
    pub kind: String,
}

pub fn write_schema(output: Option<&Path>) -> anyhow::Result<u8> {
    let bundle = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "manifest": schema_for!(Manifest),
        "summary": schema_for!(Summary),
        "comparison": schema_for!(Comparison),
    });
    let serialized = serde_json::to_string_pretty(&bundle)?;
    if let Some(path) = output {
        std::fs::write(path, format!("{serialized}\n"))
            .with_context(|| format!("failed to write schema to {}", path.display()))?;
    } else {
        println!("{serialized}");
    }
    Ok(0)
}

pub fn manifest_path(run_dir: &Path) -> PathBuf {
    run_dir.join("manifest.json")
}

pub fn summary_path(run_dir: &Path) -> PathBuf {
    run_dir.join("summary.json")
}
