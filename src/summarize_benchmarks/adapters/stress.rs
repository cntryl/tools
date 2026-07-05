use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use walkdir::WalkDir;

use super::super::classify::classify_stability;
use super::super::config::{BenchSummaryConfig, StressAdapterConfig};
use super::super::model::{BenchmarkRecord, MetricDirection};
use super::BenchmarkAdapter;

const STRESS_SCHEMA_VERSION: &str = "cntryl-stress.v1";

pub struct StressAdapter;

impl BenchmarkAdapter for StressAdapter {
    fn name(&self) -> &'static str {
        "stress"
    }

    fn collect(&self, config: &BenchSummaryConfig) -> Result<Vec<BenchmarkRecord>> {
        let Some(adapter_config) = config.adapters.stress.as_ref() else {
            return Ok(Vec::new());
        };
        if !adapter_config.input_root.exists() {
            return Ok(Vec::new());
        }

        let mut records = Vec::new();
        for path in latest_json_files(&adapter_config.input_root) {
            let text = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let run_file = parse_stress_run_file(&text, &path)?;
            records.extend(records_from_stress_run(
                &run_file,
                &path,
                config,
                adapter_config,
            )?);
        }

        Ok(records)
    }
}

fn parse_stress_run_file(text: &str, path: &Path) -> Result<StressRunFile> {
    let value: serde_json::Value = serde_json::from_str(text)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    let schema_version = value
        .get("schema_version")
        .and_then(serde_json::Value::as_str);
    if schema_version != Some(STRESS_SCHEMA_VERSION) {
        bail!(
            "unsupported stress schema in {}: schema_version={}; expected {STRESS_SCHEMA_VERSION}",
            path.display(),
            schema_version.unwrap_or("missing"),
        );
    }

    let run_file: StressRunFile = serde_json::from_value(value)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(run_file)
}

fn latest_json_files(root: &Path) -> impl Iterator<Item = std::path::PathBuf> + '_ {
    WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(walkdir::DirEntry::into_path)
        .filter(|path| path.file_name().and_then(|name| name.to_str()) == Some("latest.json"))
}

fn records_from_stress_run(
    run_file: &StressRunFile,
    path: &Path,
    config: &BenchSummaryConfig,
    adapter_config: &StressAdapterConfig,
) -> Result<Vec<BenchmarkRecord>> {
    if run_file.schema_version != STRESS_SCHEMA_VERSION {
        bail!(
            "unsupported stress schema '{}' in {}; expected {STRESS_SCHEMA_VERSION}",
            run_file.schema_version,
            path.display()
        );
    }

    let elapsed_by_benchmark = measured_elapsed_by_benchmark(&run_file.samples);
    let mut records = Vec::new();
    for summary in &run_file.summaries {
        records.extend(records_from_summary(
            run_file,
            summary,
            elapsed_by_benchmark.get(&summary.benchmark_id).copied(),
            path,
            config,
            adapter_config,
        )?);
    }
    Ok(records)
}

fn records_from_summary(
    run_file: &StressRunFile,
    summary: &StressSummary,
    median_elapsed_ns: Option<f64>,
    path: &Path,
    config: &BenchSummaryConfig,
    adapter_config: &StressAdapterConfig,
) -> Result<Vec<BenchmarkRecord>> {
    let context = StressRecordContext {
        run_file,
        median_elapsed_ns,
        path,
        config,
        adapter_config,
    };
    let mut records = Vec::new();
    if let Some(stats) = summary.stats.as_ref() {
        let metric = metric_spec(&summary.primary_metric)?;
        if let Some(record) = record_from_stats(&context, summary, metric, stats) {
            records.push(record);
        }
    }

    for (metric_name, stats) in [
        ("ns_per_op", summary.ns_per_op.as_ref()),
        ("allocs_per_op", summary.allocs_per_op.as_ref()),
        ("bytes_per_op", summary.bytes_per_op.as_ref()),
    ] {
        if summary.primary_metric == metric_name {
            continue;
        }
        let Some(stats) = stats else {
            continue;
        };
        let metric = metric_spec(metric_name)?;
        if let Some(record) = record_from_stats(&context, summary, metric, stats) {
            records.push(record);
        }
    }

    Ok(records)
}

struct StressRecordContext<'a> {
    run_file: &'a StressRunFile,
    median_elapsed_ns: Option<f64>,
    path: &'a Path,
    config: &'a BenchSummaryConfig,
    adapter_config: &'a StressAdapterConfig,
}

fn record_from_stats(
    context: &StressRecordContext<'_>,
    summary: &StressSummary,
    metric: StressMetricSpec,
    stats: &StressStats,
) -> Option<BenchmarkRecord> {
    let value = primary_value(metric.primary_metric, stats)?;
    if !value.is_finite()
        || value < 0.0
        || (value == 0.0
            && !matches!(
                metric.primary_metric,
                StressPrimaryMetric::AllocsPerOp | StressPrimaryMetric::BytesPerOp
            ))
    {
        return None;
    }
    if metric.primary_metric == StressPrimaryMetric::Throughput
        && value > context.adapter_config.max_reasonable_throughput_ops_per_s
    {
        return None;
    }

    let scenario = summary
        .parameters
        .get("scenario")
        .or_else(|| summary.metadata.get("scenario"))
        .cloned();
    let tags = summary.parameters.clone();
    let mut metadata = summary_metadata(
        context.run_file,
        summary,
        stats,
        metric.name,
        context.median_elapsed_ns,
    );
    metadata.insert(
        "meets_sample_floor".to_string(),
        (summary.measured_samples >= context.adapter_config.authoritative_min_samples).to_string(),
    );
    if let Some(median_elapsed_ns) = context.median_elapsed_ns {
        metadata.insert(
            "meets_runtime_floor".to_string(),
            (median_elapsed_ns >= context.adapter_config.min_reasonable_duration_ns).to_string(),
        );
    }
    for (key, value) in &summary.metadata {
        metadata.insert(format!("stress_metadata_{key}"), value.clone());
    }

    Some(BenchmarkRecord {
        id: record_id(summary, metric.name, scenario.as_deref()),
        adapter: "stress".to_string(),
        suite: context.run_file.suite.clone(),
        case: stress_case_name(&summary.name),
        scenario,
        metric: metric.name.to_string(),
        unit: metric.unit.to_string(),
        value,
        lower_bound: Some(stats.min),
        upper_bound: Some(stats.max),
        samples: Some(summary.measured_samples),
        metric_direction: metric.direction,
        stability: Some(classify_stability(
            stats.relative_std_dev,
            &context.config.stability_thresholds,
        )),
        status: Some(summary_status(summary)),
        rel_stddev: stats.relative_std_dev,
        tags,
        metadata,
        source_file: context.path.display().to_string(),
    })
}

fn summary_status(summary: &StressSummary) -> String {
    if summary.correctness.passed {
        summary.quality.clone()
    } else {
        "correctness_failed".to_string()
    }
}

fn summary_metadata(
    run_file: &StressRunFile,
    summary: &StressSummary,
    stats: &StressStats,
    record_metric: &str,
    median_elapsed_ns: Option<f64>,
) -> BTreeMap<String, String> {
    let mut metadata = BTreeMap::from([
        ("benchmark_id".to_string(), summary.benchmark_id.clone()),
        ("name".to_string(), summary.name.clone()),
        ("tier".to_string(), summary.tier.to_string()),
        ("primary_metric".to_string(), summary.primary_metric.clone()),
        ("record_metric".to_string(), record_metric.to_string()),
        ("quality".to_string(), summary.quality.clone()),
        ("run_profile".to_string(), run_file.run_profile.clone()),
        (
            "measured_samples".to_string(),
            summary.measured_samples.to_string(),
        ),
        (
            "warmup_samples".to_string(),
            summary.warmup_samples.to_string(),
        ),
        (
            "cooldown_samples".to_string(),
            summary.cooldown_samples.to_string(),
        ),
        (
            "correctness_passed".to_string(),
            summary.correctness.passed.to_string(),
        ),
        (
            "correctness_errors".to_string(),
            summary.correctness.errors.join(","),
        ),
        ("stats_mean".to_string(), stats.mean.to_string()),
        ("stats_median".to_string(), stats.median.to_string()),
        ("stats_min".to_string(), stats.min.to_string()),
        ("stats_max".to_string(), stats.max.to_string()),
        ("stats_std_dev".to_string(), stats.std_dev.to_string()),
        (
            "stats_relative_std_dev".to_string(),
            stats
                .relative_std_dev
                .map_or_else(|| "null".to_string(), |value| value.to_string()),
        ),
        (
            "ci95_lower".to_string(),
            stats.confidence_interval_95.lower.to_string(),
        ),
        (
            "ci95_upper".to_string(),
            stats.confidence_interval_95.upper.to_string(),
        ),
        ("p50".to_string(), stats.p50.to_string()),
        ("p95".to_string(), stats.p95.to_string()),
        ("p99".to_string(), stats.p99.to_string()),
    ]);
    if !summary.flags.is_empty() {
        metadata.insert("flags".to_string(), summary.flags.join(","));
    }
    if let Some(overhead) = &summary.overhead_ns_per_op {
        metadata.insert(
            "overhead_ns_per_op_mean".to_string(),
            overhead.mean.to_string(),
        );
    }
    if let Some(median_elapsed_ns) = median_elapsed_ns {
        metadata.insert(
            "median_sample_elapsed_ns".to_string(),
            median_elapsed_ns.to_string(),
        );
    }
    metadata.extend(summary.correctness.counters.to_metadata());
    metadata
}

fn record_id(summary: &StressSummary, metric: &str, scenario: Option<&str>) -> String {
    let mut parts = vec![summary.benchmark_id.clone(), metric.to_string()];
    if let Some(scenario) = scenario {
        parts.push(format!("scenario={scenario}"));
    }
    parts.extend(
        summary
            .parameters
            .iter()
            .filter(|(key, _)| key.as_str() != "scenario")
            .map(|(key, value)| format!("{key}={value}")),
    );
    parts.join("|")
}

fn measured_elapsed_by_benchmark(samples: &[StressSample]) -> BTreeMap<String, f64> {
    let mut grouped = BTreeMap::<String, Vec<f64>>::new();
    for sample in samples
        .iter()
        .filter(|sample| sample.phase == StressSamplePhase::Measured)
    {
        grouped
            .entry(sample.benchmark_id.clone())
            .or_default()
            .push(sample.elapsed_ns);
    }
    grouped
        .into_iter()
        .filter_map(|(benchmark_id, values)| median(&values).map(|value| (benchmark_id, value)))
        .collect()
}

fn stress_case_name(name: &str) -> String {
    name.split("::").last().unwrap_or(name).to_string()
}

fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut ordered = values.to_vec();
    ordered.sort_by(|left, right| compare_f64(*left, *right));
    let mid = ordered.len() / 2;
    Some(if ordered.len().is_multiple_of(2) {
        f64::midpoint(ordered[mid - 1], ordered[mid])
    } else {
        ordered[mid]
    })
}

fn compare_f64(left: f64, right: f64) -> Ordering {
    left.partial_cmp(&right).unwrap_or(Ordering::Equal)
}

fn metric_spec(primary_metric: &str) -> Result<StressMetricSpec> {
    match primary_metric {
        "throughput" => Ok(StressMetricSpec {
            primary_metric: StressPrimaryMetric::Throughput,
            name: "throughput_ops_per_s",
            unit: "ops/s",
            direction: MetricDirection::HigherIsBetter,
        }),
        "latency_p95" => Ok(StressMetricSpec {
            primary_metric: StressPrimaryMetric::LatencyP95,
            name: "latency_p95_ns",
            unit: "ns",
            direction: MetricDirection::LowerIsBetter,
        }),
        "ns_per_op" => Ok(StressMetricSpec {
            primary_metric: StressPrimaryMetric::NsPerOp,
            name: "ns_per_op",
            unit: "ns",
            direction: MetricDirection::LowerIsBetter,
        }),
        "allocs_per_op" => Ok(StressMetricSpec {
            primary_metric: StressPrimaryMetric::AllocsPerOp,
            name: "allocs_per_op",
            unit: "allocs/op",
            direction: MetricDirection::LowerIsBetter,
        }),
        "bytes_per_op" => Ok(StressMetricSpec {
            primary_metric: StressPrimaryMetric::BytesPerOp,
            name: "bytes_per_op",
            unit: "B/op",
            direction: MetricDirection::LowerIsBetter,
        }),
        other => bail!("unknown stress primary metric '{other}'"),
    }
}

fn primary_value(metric: StressPrimaryMetric, stats: &StressStats) -> Option<f64> {
    let value = match metric {
        StressPrimaryMetric::Throughput
        | StressPrimaryMetric::NsPerOp
        | StressPrimaryMetric::AllocsPerOp
        | StressPrimaryMetric::BytesPerOp => stats.mean,
        StressPrimaryMetric::LatencyP95 => stats.p95,
    };
    value.is_finite().then_some(value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StressPrimaryMetric {
    Throughput,
    LatencyP95,
    NsPerOp,
    AllocsPerOp,
    BytesPerOp,
}

#[derive(Debug, Clone, Copy)]
struct StressMetricSpec {
    primary_metric: StressPrimaryMetric,
    name: &'static str,
    unit: &'static str,
    direction: MetricDirection,
}

#[derive(Debug, Deserialize)]
struct StressRunFile {
    schema_version: String,
    suite: String,
    run_profile: String,
    #[serde(default)]
    summaries: Vec<StressSummary>,
    #[serde(default)]
    samples: Vec<StressSample>,
}

#[derive(Debug, Deserialize)]
struct StressSummary {
    benchmark_id: String,
    name: String,
    tier: u32,
    primary_metric: String,
    measured_samples: usize,
    warmup_samples: usize,
    cooldown_samples: usize,
    stats: Option<StressStats>,
    ns_per_op: Option<StressStats>,
    overhead_ns_per_op: Option<StressStats>,
    allocs_per_op: Option<StressStats>,
    bytes_per_op: Option<StressStats>,
    quality: String,
    correctness: StressCorrectness,
    #[serde(default)]
    flags: Vec<String>,
    #[serde(default)]
    parameters: BTreeMap<String, String>,
    #[serde(default)]
    metadata: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct StressStats {
    mean: f64,
    median: f64,
    min: f64,
    max: f64,
    std_dev: f64,
    relative_std_dev: Option<f64>,
    confidence_interval_95: StressConfidenceInterval,
    p50: f64,
    p95: f64,
    p99: f64,
}

#[derive(Debug, Deserialize)]
struct StressConfidenceInterval {
    lower: f64,
    upper: f64,
}

#[derive(Debug, Deserialize)]
struct StressCorrectness {
    passed: bool,
    #[serde(default)]
    counters: StressCorrectnessCounters,
    #[serde(default)]
    errors: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
struct StressCorrectnessCounters {
    attempted: u64,
    completed: u64,
    failures: u64,
    timeouts: u64,
    duplicates: u64,
    dropped: u64,
    validation_errors: u64,
}

impl StressCorrectnessCounters {
    fn to_metadata(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            (
                "correctness_attempted".to_string(),
                self.attempted.to_string(),
            ),
            (
                "correctness_completed".to_string(),
                self.completed.to_string(),
            ),
            (
                "correctness_failures".to_string(),
                self.failures.to_string(),
            ),
            (
                "correctness_timeouts".to_string(),
                self.timeouts.to_string(),
            ),
            (
                "correctness_duplicates".to_string(),
                self.duplicates.to_string(),
            ),
            ("correctness_dropped".to_string(), self.dropped.to_string()),
            (
                "correctness_validation_errors".to_string(),
                self.validation_errors.to_string(),
            ),
        ])
    }
}

#[derive(Debug, Deserialize)]
struct StressSample {
    benchmark_id: String,
    phase: StressSamplePhase,
    elapsed_ns: f64,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum StressSamplePhase {
    Warmup,
    Measured,
    Cooldown,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn config() -> BenchSummaryConfig {
        BenchSummaryConfig::for_tests()
    }

    fn adapter_config(config: &BenchSummaryConfig) -> &StressAdapterConfig {
        config.adapters.stress.as_ref().expect("stress config")
    }

    fn parse_run(json: &str) -> StressRunFile {
        serde_json::from_str(json).expect("valid stress run")
    }

    #[test]
    fn should_reject_stress_artifact_without_v1_schema() {
        let result = parse_stress_run_file(
            r#"{"suite": "old", "results": []}"#,
            &PathBuf::from("target/stress/old/latest.json"),
        );

        assert!(result.is_err());
    }

    #[test]
    fn should_fail_malformed_v1_stress_artifact() {
        let result = parse_stress_run_file(
            r#"{"schema_version": "cntryl-stress.v1"}"#,
            &PathBuf::from("target/stress/bad/latest.json"),
        );

        assert!(result.is_err());
    }

    #[test]
    fn should_collect_v1_throughput_summary() {
        let config = config();
        let run = parse_run(
            r#"{
                "schema_version": "cntryl-stress.v1",
                "suite": "tier4_queue",
                "run_profile": "release",
                "samples": [
                    {"benchmark_id": "tier4_queue/queue::fanout", "phase": "warmup", "elapsed_ns": 1000000000},
                    {"benchmark_id": "tier4_queue/queue::fanout", "phase": "measured", "elapsed_ns": 3000000000},
                    {"benchmark_id": "tier4_queue/queue::fanout", "phase": "measured", "elapsed_ns": 5000000000}
                ],
                "summaries": [{
                    "benchmark_id": "tier4_queue/queue::fanout",
                    "name": "queue::fanout",
                    "tier": 4,
                    "primary_metric": "throughput",
                    "measured_samples": 10,
                    "warmup_samples": 1,
                    "cooldown_samples": 0,
                    "stats": {
                        "mean": 1000.0,
                        "median": 995.0,
                        "min": 900.0,
                        "max": 1100.0,
                        "std_dev": 40.0,
                        "relative_std_dev": 0.04,
                        "confidence_interval_95": {"lower": 975.0, "upper": 1025.0},
                        "p50": 995.0,
                        "p95": 1080.0,
                        "p99": 1095.0
                    },
                    "quality": "authoritative",
                    "correctness": {
                        "passed": true,
                        "counters": {
                            "attempted": 1000,
                            "completed": 1000,
                            "failures": 0,
                            "timeouts": 0,
                            "duplicates": 0,
                            "dropped": 0,
                            "validation_errors": 0
                        },
                        "errors": []
                    },
                    "parameters": {
                        "scenario": "fanout",
                        "client_count": "16",
                        "transport": "tcp"
                    },
                    "metadata": {
                        "operation": "enqueue"
                    }
                }]
            }"#,
        );

        let records = records_from_stress_run(
            &run,
            &PathBuf::from("target/stress/tier4_queue/latest.json"),
            &config,
            adapter_config(&config),
        )
        .expect("records");

        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.id, "tier4_queue/queue::fanout|throughput_ops_per_s|scenario=fanout|client_count=16|transport=tcp");
        assert_eq!(record.suite, "tier4_queue");
        assert_eq!(record.case, "fanout");
        assert_eq!(record.scenario, Some("fanout".to_string()));
        assert_eq!(record.metric, "throughput_ops_per_s");
        assert_eq!(record.unit, "ops/s");
        assert_eq!(record.metric_direction, MetricDirection::HigherIsBetter);
        assert_close(record.value, 1000.0);
        assert_optional_close(record.lower_bound, 900.0);
        assert_optional_close(record.upper_bound, 1100.0);
        assert_eq!(record.samples, Some(10));
        assert_eq!(record.status, Some("authoritative".to_string()));
        assert_eq!(record.stability, Some("stable".to_string()));
        assert_eq!(record.tags.get("client_count"), Some(&"16".to_string()));
        assert_eq!(
            record.metadata.get("median_sample_elapsed_ns"),
            Some(&"4000000000".to_string())
        );
        assert_eq!(
            record.metadata.get("stress_metadata_operation"),
            Some(&"enqueue".to_string())
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn should_collect_v1_micro_and_allocation_records() {
        let config = config();
        let run = parse_run(
            r#"{
                "schema_version": "cntryl-stress.v1",
                "suite": "tier1_hot_path",
                "run_profile": "release",
                "samples": [
                    {"benchmark_id": "tier1_hot_path/parser::header", "phase": "measured", "elapsed_ns": 10000000}
                ],
                "summaries": [{
                    "benchmark_id": "tier1_hot_path/parser::header",
                    "name": "parser::header",
                    "tier": 1,
                    "primary_metric": "ns_per_op",
                    "measured_samples": 10,
                    "warmup_samples": 1,
                    "cooldown_samples": 0,
                    "stats": {
                        "mean": 42.0,
                        "median": 41.0,
                        "min": 40.0,
                        "max": 45.0,
                        "std_dev": 1.0,
                        "relative_std_dev": 0.02,
                        "confidence_interval_95": {"lower": 41.0, "upper": 43.0},
                        "p50": 41.0,
                        "p95": 44.0,
                        "p99": 45.0
                    },
                    "ns_per_op": {
                        "mean": 42.0,
                        "median": 41.0,
                        "min": 40.0,
                        "max": 45.0,
                        "std_dev": 1.0,
                        "relative_std_dev": 0.02,
                        "confidence_interval_95": {"lower": 41.0, "upper": 43.0},
                        "p50": 41.0,
                        "p95": 44.0,
                        "p99": 45.0
                    },
                    "overhead_ns_per_op": {
                        "mean": 2.0,
                        "median": 2.0,
                        "min": 1.0,
                        "max": 3.0,
                        "std_dev": 0.5,
                        "relative_std_dev": 0.25,
                        "confidence_interval_95": {"lower": 1.5, "upper": 2.5},
                        "p50": 2.0,
                        "p95": 3.0,
                        "p99": 3.0
                    },
                    "allocs_per_op": {
                        "mean": 0.0,
                        "median": 0.0,
                        "min": 0.0,
                        "max": 0.0,
                        "std_dev": 0.0,
                        "relative_std_dev": 0.0,
                        "confidence_interval_95": {"lower": 0.0, "upper": 0.0},
                        "p50": 0.0,
                        "p95": 0.0,
                        "p99": 0.0
                    },
                    "bytes_per_op": {
                        "mean": 0.0,
                        "median": 0.0,
                        "min": 0.0,
                        "max": 0.0,
                        "std_dev": 0.0,
                        "relative_std_dev": 0.0,
                        "confidence_interval_95": {"lower": 0.0, "upper": 0.0},
                        "p50": 0.0,
                        "p95": 0.0,
                        "p99": 0.0
                    },
                    "quality": "authoritative",
                    "flags": ["validated_micro"],
                    "correctness": {
                        "passed": true,
                        "counters": {
                            "attempted": 1000,
                            "completed": 1000,
                            "failures": 0,
                            "timeouts": 0,
                            "duplicates": 0,
                            "dropped": 0,
                            "validation_errors": 0
                        },
                        "errors": []
                    },
                    "parameters": {"operation": "parse_header"}
                }]
            }"#,
        );

        let records = records_from_stress_run(
            &run,
            &PathBuf::from("target/stress/tier1_hot_path/latest.json"),
            &config,
            adapter_config(&config),
        )
        .expect("records");

        assert_eq!(records.len(), 3);
        assert_eq!(records[0].metric, "ns_per_op");
        assert_eq!(records[0].unit, "ns");
        assert_eq!(records[0].metric_direction, MetricDirection::LowerIsBetter);
        assert_close(records[0].value, 42.0);
        assert_eq!(
            records[0].metadata.get("overhead_ns_per_op_mean"),
            Some(&"2".to_string())
        );
        assert_eq!(
            records[0].metadata.get("flags"),
            Some(&"validated_micro".to_string())
        );
        assert_eq!(records[1].metric, "allocs_per_op");
        assert_eq!(records[1].unit, "allocs/op");
        assert_eq!(records[2].metric, "bytes_per_op");
        assert_eq!(records[2].unit, "B/op");
    }

    #[test]
    fn should_map_latency_p95_as_lower_is_better() {
        let config = config();
        let run = parse_run(
            r#"{
                "schema_version": "cntryl-stress.v1",
                "suite": "tier3_rpc",
                "run_profile": "release",
                "samples": [],
                "summaries": [{
                    "benchmark_id": "tier3_rpc/rpc::round_trip",
                    "name": "rpc::round_trip",
                    "tier": 3,
                    "primary_metric": "latency_p95",
                    "measured_samples": 10,
                    "warmup_samples": 1,
                    "cooldown_samples": 0,
                    "stats": {
                        "mean": 100.0,
                        "median": 95.0,
                        "min": 80.0,
                        "max": 180.0,
                        "std_dev": 5.0,
                        "relative_std_dev": 0.05,
                        "confidence_interval_95": {"lower": 95.0, "upper": 105.0},
                        "p50": 95.0,
                        "p95": 160.0,
                        "p99": 180.0
                    },
                    "quality": "acceptable",
                    "correctness": {
                        "passed": true,
                        "counters": {
                            "attempted": 10,
                            "completed": 10,
                            "failures": 0,
                            "timeouts": 0,
                            "duplicates": 0,
                            "dropped": 0,
                            "validation_errors": 0
                        },
                        "errors": []
                    }
                }]
            }"#,
        );

        let records = records_from_stress_run(
            &run,
            &PathBuf::from("target/stress/tier3_rpc/latest.json"),
            &config,
            adapter_config(&config),
        )
        .expect("records");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].metric, "latency_p95_ns");
        assert_eq!(records[0].unit, "ns");
        assert_eq!(records[0].metric_direction, MetricDirection::LowerIsBetter);
        assert_close(records[0].value, 160.0);
        assert_eq!(records[0].status, Some("acceptable".to_string()));
    }

    #[test]
    fn should_reject_non_current_stress_schema() {
        let config = config();
        let run = parse_run(
            r#"{
                "schema_version": "cntryl-stress.v2",
                "suite": "old",
                "run_profile": "smoke",
                "summaries": [],
                "samples": []
            }"#,
        );

        let result = records_from_stress_run(
            &run,
            &PathBuf::from("target/stress/old/latest.json"),
            &config,
            adapter_config(&config),
        );

        assert!(result.is_err());
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < f64::EPSILON,
            "expected {actual} to equal {expected}"
        );
    }

    fn assert_optional_close(actual: Option<f64>, expected: f64) {
        assert_close(actual.expect("value"), expected);
    }
}
