use std::collections::BTreeMap;
use std::fs::File;

use anyhow::Context;
use serde::Deserialize;

use crate::cli::CompareArgs;
use crate::contract::{
    BudgetViolation, CollectorSet, Comparison, Manifest, MetricDelta, Summary, manifest_path,
    summary_path,
};
use crate::{SCHEMA_VERSION, run, util};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct BudgetConfig {
    #[serde(default)]
    budgets: BTreeMap<String, Budget>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Budget {
    max: Option<f64>,
    regression_percent: Option<f64>,
}

pub async fn execute(args: CompareArgs) -> anyhow::Result<u8> {
    let baseline: Summary =
        serde_json::from_reader(File::open(summary_path(&args.baseline)).with_context(|| {
            format!("missing baseline summary in {}", args.baseline.display())
        })?)?;
    let candidate: Summary =
        serde_json::from_reader(File::open(summary_path(&args.candidate)).with_context(|| {
            format!("missing candidate summary in {}", args.candidate.display())
        })?)?;
    let baseline_manifest: Manifest =
        serde_json::from_reader(File::open(manifest_path(&args.baseline)).with_context(|| {
            format!("missing baseline manifest in {}", args.baseline.display())
        })?)?;
    let mut candidate_manifest: Manifest =
        serde_json::from_reader(File::open(manifest_path(&args.candidate)).with_context(
            || format!("missing candidate manifest in {}", args.candidate.display()),
        )?)?;
    util::verify_artifacts(&args.baseline, &baseline_manifest.files)
        .context("baseline artifact integrity check failed")?;
    util::verify_artifacts(&args.candidate, &candidate_manifest.files)
        .context("candidate artifact integrity check failed")?;
    let config_text = std::fs::read_to_string(&args.config)
        .with_context(|| format!("failed to read budget config {}", args.config.display()))?;
    let config = toml::from_str::<BudgetConfig>(&config_text)
        .with_context(|| format!("invalid budget config {}", args.config.display()))?;

    validate_budget_names(&config.budgets)?;
    let mut reasons = comparability_reasons(
        &baseline,
        &candidate,
        &baseline_manifest,
        &candidate_manifest,
    );
    reasons.extend(run_contract_reasons(
        &baseline,
        &baseline_manifest,
        "baseline",
    ));
    reasons.extend(run_contract_reasons(
        &candidate,
        &candidate_manifest,
        "candidate",
    ));
    let metrics = metric_deltas(
        &baseline,
        &candidate,
        baseline_manifest.collectors,
        candidate_manifest.collectors,
    );
    for name in config.budgets.keys() {
        if !metrics.contains_key(name) {
            reasons.push(format!(
                "budget metric {name} is unavailable for these collectors"
            ));
        }
    }
    let mut comparison = Comparison {
        schema_version: SCHEMA_VERSION,
        comparable: reasons.is_empty(),
        reasons,
        metrics,
        violations: Vec::new(),
    };
    if comparison.comparable {
        comparison.violations = apply_budgets(&comparison.metrics, &config.budgets);
    }
    util::atomic_write_json(&args.candidate.join("comparison.json"), &comparison)?;
    run::finalize_manifest(&args.candidate, &mut candidate_manifest)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&comparison)?);
    } else {
        print_comparison(&comparison);
    }
    if !comparison.comparable {
        Ok(70)
    } else if comparison.violations.is_empty() {
        Ok(0)
    } else {
        Ok(10)
    }
}

fn run_contract_reasons(summary: &Summary, manifest: &Manifest, label: &str) -> Vec<String> {
    let mut reasons = Vec::new();
    if manifest.schema_version != SCHEMA_VERSION || summary.schema_version != SCHEMA_VERSION {
        reasons.push(format!("{label} schema version is unsupported"));
    }
    if summary.run_id != manifest.run_id {
        reasons.push(format!("{label} summary run_id differs from manifest"));
    }
    let paths = manifest
        .files
        .iter()
        .map(|artifact| artifact.path.as_str())
        .collect::<Vec<_>>();
    if manifest.collectors.telemetry
        && (!manifest.completeness.telemetry || !paths.contains(&"telemetry.ndjson"))
    {
        reasons.push(format!("{label} telemetry collector is incomplete"));
    }
    if manifest.collectors.cpu
        && (!manifest.completeness.cpu
            || !manifest
                .files
                .iter()
                .any(|artifact| artifact.kind == "v8_cpu_profile"))
    {
        reasons.push(format!("{label} CPU collector is incomplete"));
    }
    if manifest.collectors.heap
        && (!manifest.completeness.heap
            || !manifest
                .files
                .iter()
                .any(|artifact| artifact.kind == "v8_heap_profile"))
    {
        reasons.push(format!("{label} heap collector is incomplete"));
    }
    if manifest.collectors.asynchronous
        && (!manifest.completeness.asynchronous || !paths.contains(&"profiles/async/events.ndjson"))
    {
        reasons.push(format!("{label} async collector is incomplete"));
    }
    reasons
}

fn comparability_reasons(
    baseline: &Summary,
    candidate: &Summary,
    baseline_manifest: &Manifest,
    candidate_manifest: &Manifest,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if baseline_manifest.finished_at.is_none() || candidate_manifest.finished_at.is_none() {
        reasons.push("run has not finished".into());
    }
    if baseline_manifest.completeness.partial || candidate_manifest.completeness.partial {
        reasons.push("run is partial".into());
    }
    if baseline_manifest.mode != candidate_manifest.mode {
        reasons.push(format!(
            "mode differs: {:?} vs {:?}",
            baseline_manifest.mode, candidate_manifest.mode
        ));
    }
    if baseline_manifest.collectors != candidate_manifest.collectors {
        reasons.push("collector set differs".into());
    }
    for (label, left, right) in [
        (
            "Node major",
            baseline
                .comparability
                .node_major
                .map(|value| value.to_string()),
            candidate
                .comparability
                .node_major
                .map(|value| value.to_string()),
        ),
        (
            "V8 major",
            baseline
                .comparability
                .v8_major
                .map(|value| value.to_string()),
            candidate
                .comparability
                .v8_major
                .map(|value| value.to_string()),
        ),
        (
            "OS",
            non_empty(&baseline.comparability.os),
            non_empty(&candidate.comparability.os),
        ),
        (
            "architecture",
            non_empty(&baseline.comparability.arch),
            non_empty(&candidate.comparability.arch),
        ),
    ] {
        match (&left, &right) {
            (None, None) => reasons.push(format!("{label} is missing from baseline and candidate")),
            (None, Some(_)) => reasons.push(format!("{label} is missing from baseline")),
            (Some(_), None) => reasons.push(format!("{label} is missing from candidate")),
            (Some(left), Some(right)) if left != right => {
                reasons.push(format!("{label} differs: {left} vs {right}"));
            }
            (Some(_), Some(_)) => {}
        }
    }
    reasons
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn metric_deltas(
    baseline: &Summary,
    candidate: &Summary,
    baseline_collectors: CollectorSet,
    candidate_collectors: CollectorSet,
) -> BTreeMap<String, MetricDelta> {
    let baseline_values = metrics(baseline, baseline_collectors);
    let candidate_values = metrics(candidate, candidate_collectors);
    baseline_values
        .into_iter()
        .filter_map(|(name, (left, unit))| {
            let (right, _) = candidate_values.get(&name)?;
            let delta = *right - left;
            Some((
                name,
                MetricDelta {
                    baseline: left,
                    candidate: *right,
                    delta,
                    percent: (left != 0.0).then_some(delta / left * 100.0),
                    unit,
                },
            ))
        })
        .collect()
}

fn metrics(summary: &Summary, collectors: CollectorSet) -> BTreeMap<String, (f64, String)> {
    let heap_growth =
        summary.memory.heap_used_end_bytes as f64 - summary.memory.heap_used_start_bytes as f64;
    if !collectors.telemetry {
        return BTreeMap::new();
    }
    BTreeMap::from([
        (
            "event_loop_p99_ms".into(),
            (summary.event_loop.delay_p99_ms, "ms".into()),
        ),
        (
            "event_loop_max_ms".into(),
            (summary.event_loop.delay_max_ms, "ms".into()),
        ),
        (
            "event_loop_utilization".into(),
            (summary.event_loop.utilization_avg, "ratio".into()),
        ),
        (
            "cpu_avg_percent".into(),
            (summary.cpu.process_cpu_avg_percent, "percent".into()),
        ),
        (
            "rss_max_bytes".into(),
            (summary.memory.rss_max_bytes as f64, "bytes".into()),
        ),
        (
            "heap_used_max_bytes".into(),
            (summary.memory.heap_used_max_bytes as f64, "bytes".into()),
        ),
        ("heap_growth_bytes".into(), (heap_growth, "bytes".into())),
        (
            "gc_total_pause_ms".into(),
            (summary.gc.total_pause_ms, "ms".into()),
        ),
        (
            "gc_max_pause_ms".into(),
            (summary.gc.max_pause_ms, "ms".into()),
        ),
        (
            "gc_max_blocking_ms_per_second".into(),
            (summary.gc.max_blocking_ms_per_second, "ms".into()),
        ),
        (
            "active_resources_final".into(),
            (
                summary.resources.final_counts.values().sum::<u64>() as f64,
                "count".into(),
            ),
        ),
    ])
}

fn validate_budget_names(budgets: &BTreeMap<String, Budget>) -> anyhow::Result<()> {
    const METRICS: &[&str] = &[
        "event_loop_p99_ms",
        "event_loop_max_ms",
        "event_loop_utilization",
        "cpu_avg_percent",
        "rss_max_bytes",
        "heap_used_max_bytes",
        "heap_growth_bytes",
        "gc_total_pause_ms",
        "gc_max_pause_ms",
        "gc_max_blocking_ms_per_second",
        "active_resources_final",
    ];
    if budgets.is_empty() {
        anyhow::bail!("budget configuration contains no budgets");
    }
    for (name, budget) in budgets {
        if !METRICS.contains(&name.as_str()) {
            anyhow::bail!("unknown budget metric {name}");
        }
        if budget.max.is_none() && budget.regression_percent.is_none() {
            anyhow::bail!("budget {name} has no max or regression_percent constraint");
        }
        if budget.max.is_some_and(|value| !value.is_finite()) {
            anyhow::bail!("budget {name} max must be finite");
        }
        if budget
            .regression_percent
            .is_some_and(|value| !value.is_finite() || value < 0.0)
        {
            anyhow::bail!("budget {name} regression_percent must be finite and non-negative");
        }
    }
    Ok(())
}

fn apply_budgets(
    metrics: &BTreeMap<String, MetricDelta>,
    budgets: &BTreeMap<String, Budget>,
) -> Vec<BudgetViolation> {
    let mut violations = Vec::new();
    for (name, budget) in budgets {
        let Some(metric) = metrics.get(name) else {
            continue;
        };
        if let Some(limit) = budget.max
            && metric.candidate > limit
        {
            violations.push(BudgetViolation {
                metric: name.clone(),
                observed: metric.candidate,
                limit,
                kind: "absolute_max".into(),
            });
        }
        if let Some(limit) = budget.regression_percent {
            if let Some(percent) = metric.percent
                && percent > limit
            {
                violations.push(BudgetViolation {
                    metric: name.clone(),
                    observed: percent,
                    limit,
                    kind: "regression_percent".into(),
                });
            } else if metric.percent.is_none() && metric.candidate > metric.baseline {
                violations.push(BudgetViolation {
                    metric: name.clone(),
                    observed: metric.candidate,
                    limit: metric.baseline,
                    kind: "regression_from_zero".into(),
                });
            }
        }
    }
    violations
}

fn print_comparison(comparison: &Comparison) {
    if !comparison.comparable {
        println!("Runs are not comparable:");
        for reason in &comparison.reasons {
            println!("  - {reason}");
        }
        return;
    }
    println!(
        "{:<28} {:>14} {:>14} {:>12}",
        "Metric", "Baseline", "Candidate", "Delta"
    );
    for (name, metric) in &comparison.metrics {
        let delta = metric
            .percent
            .map(|value| format!("{value:+.2}%"))
            .unwrap_or_else(|| format!("{:+.2}", metric.delta));
        println!(
            "{name:<28} {:>14.2} {:>14.2} {:>12}",
            metric.baseline, metric.candidate, delta
        );
    }
    if !comparison.violations.is_empty() {
        println!("\nBudget violations:");
        for violation in &comparison.violations {
            println!(
                "  - {}: {:.2} exceeds {:.2} ({})",
                violation.metric, violation.observed, violation.limit, violation.kind
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_absolute_and_regression_budgets() {
        let metrics = BTreeMap::from([(
            "latency".into(),
            MetricDelta {
                baseline: 10.0,
                candidate: 12.0,
                delta: 2.0,
                percent: Some(20.0),
                unit: "ms".into(),
            },
        )]);
        let budgets = BTreeMap::from([(
            "latency".into(),
            Budget {
                max: Some(11.0),
                regression_percent: Some(10.0),
            },
        )]);
        assert_eq!(apply_budgets(&metrics, &budgets).len(), 2);
    }

    #[test]
    fn rejects_unknown_and_empty_budgets() {
        assert!(validate_budget_names(&BTreeMap::new()).is_err());
        let unknown = BTreeMap::from([("typo_metric".into(), Budget::default())]);
        assert!(validate_budget_names(&unknown).is_err());
        let empty = BTreeMap::from([("event_loop_p99_ms".into(), Budget::default())]);
        assert!(validate_budget_names(&empty).is_err());
        let nan = BTreeMap::from([(
            "event_loop_p99_ms".into(),
            Budget {
                max: Some(f64::NAN),
                regression_percent: None,
            },
        )]);
        assert!(validate_budget_names(&nan).is_err());
    }

    #[test]
    fn increase_from_zero_is_a_regression() {
        let metrics = BTreeMap::from([(
            "gc_total_pause_ms".into(),
            MetricDelta {
                baseline: 0.0,
                candidate: 1.0,
                delta: 1.0,
                percent: None,
                unit: "ms".into(),
            },
        )]);
        let budgets = BTreeMap::from([(
            "gc_total_pause_ms".into(),
            Budget {
                max: None,
                regression_percent: Some(0.0),
            },
        )]);
        assert_eq!(
            apply_budgets(&metrics, &budgets)[0].kind,
            "regression_from_zero"
        );
    }

    #[test]
    fn rejects_missing_comparability_identity() {
        let summary = Summary::default();
        let now = chrono::Utc::now();
        let manifest = Manifest {
            schema_version: SCHEMA_VERSION,
            v8scope_version: crate::VERSION.into(),
            run_id: "run".into(),
            name: "run".into(),
            mode: crate::contract::Mode::Diagnose,
            collectors: CollectorSet::default(),
            started_at: now,
            finished_at: Some(now),
            command: Vec::new(),
            cwd: String::new(),
            redact_paths: true,
            platform: crate::contract::PlatformInfo::default(),
            runtime: crate::contract::RuntimeInfo::default(),
            process: crate::contract::ProcessResult::default(),
            completeness: crate::contract::Completeness::default(),
            files: Vec::new(),
        };
        let reasons = comparability_reasons(&summary, &summary, &manifest, &manifest);
        assert!(
            reasons
                .iter()
                .any(|reason| reason.contains("Node major is missing"))
        );
        assert!(
            reasons
                .iter()
                .any(|reason| reason.contains("V8 major is missing"))
        );
        assert!(
            reasons
                .iter()
                .any(|reason| reason.contains("OS is missing"))
        );
        assert!(
            reasons
                .iter()
                .any(|reason| reason.contains("architecture is missing"))
        );
    }
}
