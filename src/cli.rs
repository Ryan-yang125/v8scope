use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "v8scope",
    version,
    about = "A maintained Rust-first replacement for Clinic.js"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Diagnose event-loop, CPU, memory, GC, and active-resource behavior.
    Diagnose(RunArgs),
    /// Capture and analyze a V8 CPU profile.
    Cpu(RunArgs),
    /// Capture and analyze a V8 sampling heap profile.
    Heap(RunArgs),
    /// Capture a high-overhead async resource causality graph.
    Async(RunArgs),
    /// Capture every supported profile in one run.
    All(RunArgs),
    /// Profile a running Node.js process with an enabled Inspector endpoint.
    Attach(AttachArgs),
    /// Rebuild summary and report from a run directory.
    Analyze(AnalyzeArgs),
    /// Open a generated report in the default browser.
    Open(OpenArgs),
    /// Compare a baseline run with a candidate and apply CI budgets.
    Compare(CompareArgs),
    /// Remove old V8Scope runs.
    Clean(CleanArgs),
    /// Print or write the public JSON schemas.
    Schema(SchemaArgs),
}

#[derive(Debug, Clone, Args)]
pub struct RunArgs {
    /// Root directory for generated runs.
    #[arg(long, default_value = ".v8scope")]
    pub output: PathBuf,

    /// Human-readable run name.
    #[arg(long)]
    pub name: Option<String>,

    /// Open the report after collection.
    #[arg(long, default_value_t = false)]
    pub open: bool,

    /// Collect data without generating report HTML.
    #[arg(long)]
    pub no_report: bool,

    /// Stop the target after this duration.
    #[arg(long, value_parser = parse_duration)]
    pub duration: Option<Duration>,

    /// Runtime telemetry sampling interval.
    #[arg(long, default_value = "100ms", value_parser = parse_duration)]
    pub sample_interval: Duration,

    /// V8 CPU sampling interval in microseconds.
    #[arg(long, default_value_t = 1000)]
    pub cpu_interval: u32,

    /// V8 heap average sampling interval in bytes.
    #[arg(long, default_value_t = 524_288)]
    pub heap_interval: u64,

    /// URL polled until the target is ready.
    #[arg(long)]
    pub ready_url: Option<String>,

    /// Command launched after the readiness probe succeeds.
    #[arg(long, requires = "ready_url")]
    pub on_ready: Option<String>,

    /// URL to load after the readiness probe succeeds.
    #[arg(long, requires = "ready_url")]
    pub load_url: Option<String>,

    /// Concurrent HTTP load workers.
    #[arg(long, default_value_t = 10, requires = "load_url")]
    pub connections: usize,

    /// Optional target request rate per second.
    #[arg(long, requires = "load_url")]
    pub rate: Option<u64>,

    /// HTTP load duration.
    #[arg(long, default_value = "10s", value_parser = parse_duration, requires = "load_url")]
    pub load_duration: Duration,

    /// Maximum async events retained before events are counted as dropped.
    #[arg(long, default_value_t = 1_000_000)]
    pub async_max_events: u64,

    /// Replace absolute project paths with <project> in analyzed output.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub redact_paths: bool,

    /// Target command. Use `-- node app.js`.
    #[arg(last = true, required = true, num_args = 1.., allow_hyphen_values = true)]
    pub command: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub struct AttachArgs {
    /// Inspector HTTP discovery URL or WebSocket URL.
    #[arg(long)]
    pub url: String,

    #[arg(long, value_enum, default_value_t = AttachMode::Cpu)]
    pub mode: AttachMode,

    #[arg(long, default_value = "30s", value_parser = parse_duration)]
    pub duration: Duration,

    #[arg(long, default_value = ".v8scope")]
    pub output: PathBuf,

    #[arg(long)]
    pub name: Option<String>,

    /// Permit a non-loopback Inspector endpoint.
    #[arg(long)]
    pub allow_remote_inspector: bool,

    /// Capture a full heap snapshot in addition to a sampling profile.
    #[arg(long)]
    pub heap_snapshot: bool,

    #[arg(long, default_value_t = false)]
    pub open: bool,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum AttachMode {
    Cpu,
    Heap,
    All,
}

#[derive(Debug, Args)]
pub struct AnalyzeArgs {
    pub run_directory: PathBuf,
    #[arg(long)]
    pub no_report: bool,
}

#[derive(Debug, Args)]
pub struct OpenArgs {
    pub run_directory: PathBuf,
}

#[derive(Debug, Args)]
pub struct CompareArgs {
    pub baseline: PathBuf,
    pub candidate: PathBuf,
    #[arg(long, default_value = "v8scope.toml")]
    pub config: PathBuf,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CleanArgs {
    #[arg(long, default_value = ".v8scope")]
    pub output: PathBuf,
    #[arg(long, default_value_t = 10)]
    pub keep: usize,
}

#[derive(Debug, Args)]
pub struct SchemaArgs {
    #[arg(long)]
    pub output: Option<PathBuf>,
}

fn parse_duration(value: &str) -> Result<Duration, String> {
    humantime::parse_duration(value).map_err(|error| error.to_string())
}
