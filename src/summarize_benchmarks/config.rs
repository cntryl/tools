use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use regex::Regex;

use super::model::BenchmarkRecord;

#[derive(Debug, Clone)]
pub struct BenchSummaryConfig {
    pub product_name: String,
    pub report_title: String,
    pub root: PathBuf,
    pub baseline_file: PathBuf,
    pub manifest_file: PathBuf,
    pub markdown_report_file: PathBuf,
    pub adapters: AdapterConfigs,
    pub thresholds: SummaryThresholds,
    pub stability_thresholds: StabilityThresholds,
    pub authoritative_statuses: BTreeSet<String>,
    pub authoritative_stabilities: BTreeSet<String>,
    pub ignored_exact_ids: BTreeSet<String>,
    pub ignored_patterns: Vec<Regex>,
    pub sweep: SweepConfig,
    pub suite_display_overrides: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct AdapterConfigs {
    pub criterion: Option<CriterionAdapterConfig>,
    pub stress: Option<StressAdapterConfig>,
}

#[derive(Debug, Clone)]
pub struct CriterionAdapterConfig {
    pub input_root: PathBuf,
    pub csv_output: Option<PathBuf>,
    pub min_reasonable_mean_ns: f64,
    pub max_reasonable_mean_ns: f64,
}

#[derive(Debug, Clone)]
pub struct StressAdapterConfig {
    pub input_root: PathBuf,
    pub csv_output: Option<PathBuf>,
    pub min_reasonable_duration_ns: f64,
    pub max_reasonable_throughput_ops_per_s: f64,
    pub authoritative_min_samples: usize,
}

#[derive(Debug, Clone)]
pub struct SummaryThresholds {
    pub warning_regression_pct: f64,
    pub critical_regression_pct: f64,
}

#[derive(Debug, Clone)]
pub struct StabilityThresholds {
    pub stable: f64,
    pub acceptable: f64,
    pub noisy: f64,
}

#[derive(Debug, Clone)]
pub struct SweepConfig {
    pub tag_keys: BTreeSet<String>,
    pub tag_key_suffixes: Vec<String>,
    pub context_tag_keys: Vec<String>,
    pub name_patterns: Vec<SweepNamePatternConfig>,
}

#[derive(Debug, Clone)]
pub struct SweepNamePatternConfig {
    pub parameter: String,
    pub regex: Regex,
    pub value_capture: String,
    pub prefix_capture: Option<String>,
}

impl BenchSummaryConfig {
    pub fn for_root(
        root: PathBuf,
        product_name: impl Into<String>,
        report_title: impl Into<String>,
    ) -> Result<Self> {
        let root = root
            .canonicalize()
            .with_context(|| format!("failed to resolve repository root {}", root.display()))?;
        Self::build(&root, product_name.into(), report_title.into())
    }

    #[cfg(test)]
    pub fn for_tests() -> Self {
        Self::build(
            Path::new("."),
            "cntryl".to_string(),
            "Cntryl Benchmark Report".to_string(),
        )
        .expect("build test benchmark summary config")
    }

    #[cfg(test)]
    pub fn for_tests_with_exact_ignore<const N: usize>(ids: [&str; N]) -> Self {
        let mut config = Self::for_tests();
        config.ignored_exact_ids = ids.into_iter().map(str::to_string).collect();
        config
    }

    pub fn criterion_csv_output(&self) -> Option<&PathBuf> {
        self.adapters
            .criterion
            .as_ref()
            .and_then(|config| config.csv_output.as_ref())
    }

    pub fn stress_csv_output(&self) -> Option<&PathBuf> {
        self.adapters
            .stress
            .as_ref()
            .and_then(|config| config.csv_output.as_ref())
    }

    pub fn display_suite_label(&self, suite: &str) -> String {
        self.suite_display_overrides
            .get(suite)
            .cloned()
            .unwrap_or_else(|| suite.to_string())
    }

    pub fn should_ignore_record(&self, record: &BenchmarkRecord) -> bool {
        if self.ignored_exact_ids.contains(&record.id) {
            return true;
        }

        self.ignored_patterns
            .iter()
            .any(|pattern| Self::matches_ignore_pattern(record, pattern))
    }

    fn matches_ignore_pattern(record: &BenchmarkRecord, pattern: &Regex) -> bool {
        if pattern.is_match(&record.id)
            || pattern.is_match(&record.adapter)
            || pattern.is_match(&record.suite)
            || pattern.is_match(&record.case)
            || pattern.is_match(&record.metric)
            || pattern.is_match(&record.source_file)
        {
            return true;
        }

        if let Some(scenario) = &record.scenario {
            if pattern.is_match(scenario) {
                return true;
            }
        }

        record.tags.iter().any(|(key, value)| {
            pattern.is_match(key)
                || pattern.is_match(value)
                || pattern.is_match(&format!("{key}={value}"))
        }) || record.metadata.iter().any(|(key, value)| {
            pattern.is_match(key)
                || pattern.is_match(value)
                || pattern.is_match(&format!("{key}={value}"))
        })
    }

    fn build(root: &Path, product_name: String, report_title: String) -> Result<Self> {
        Ok(Self {
            product_name,
            report_title,
            baseline_file: root.join("config").join("bench_baseline.json"),
            manifest_file: root.join("target").join("bench_results.json"),
            markdown_report_file: root.join("target").join("bench_summary.md"),
            adapters: default_adapters(root),
            thresholds: SummaryThresholds {
                warning_regression_pct: 10.0,
                critical_regression_pct: 25.0,
            },
            stability_thresholds: default_stability_thresholds(),
            authoritative_statuses: string_set(["authoritative", "acceptable"]),
            authoritative_stabilities: string_set(["stable", "acceptable"]),
            ignored_exact_ids: default_ignored_exact_ids(),
            ignored_patterns: vec![Regex::new(r"schedule_system_scan_and_fire")?],
            sweep: default_sweep_config()?,
            suite_display_overrides: BTreeMap::new(),
            root: root.to_path_buf(),
        })
    }
}

fn default_adapters(root: &Path) -> AdapterConfigs {
    AdapterConfigs {
        criterion: Some(CriterionAdapterConfig {
            input_root: root.join("target").join("criterion"),
            csv_output: Some(
                root.join("target")
                    .join("criterion")
                    .join("summarize_benchmarks.csv"),
            ),
            min_reasonable_mean_ns: 1.0,
            max_reasonable_mean_ns: 1e12,
        }),
        stress: Some(StressAdapterConfig {
            input_root: root.join("target").join("stress"),
            csv_output: Some(root.join("target").join("stress").join("stress_summary.csv")),
            min_reasonable_duration_ns: 3e9,
            max_reasonable_throughput_ops_per_s: 1e9,
            authoritative_min_samples: 5,
        }),
    }
}

const fn default_stability_thresholds() -> StabilityThresholds {
    StabilityThresholds {
        stable: 0.05,
        acceptable: 0.10,
        noisy: 0.20,
    }
}

fn default_ignored_exact_ids() -> BTreeSet<String> {
    string_set([
        "hotpath_actor_messaging/actorref_clone_overhead",
        "hotpath_context/timer_id_new",
        "hotpath_envelope/messageid_new",
        "hotpath_envelope/metadata_extraction",
        "hotpath_envelope/is_expired_not_expired",
        "hotpath_envelope/is_expired_expired",
        "hotpath_envelope/is_expired_no_deadline",
        "hotpath_routing/full_address_from_string",
        "hotpath_routing/route_address_clone",
        "hotpath_routing/route_address_family_access",
        "hotpath_routing/route_address_route_access",
        "hotpath_routing/route_as_str",
        "hotpath_routing/route_clone_long",
        "hotpath_routing/route_clone_short",
        "hotpath_routing/route_equality_different",
        "hotpath_routing/route_equality_same",
        "hotpath_routing/route_family_from_u32",
        "hotpath_routing/route_family_new_u64",
    ])
}

fn default_sweep_config() -> Result<SweepConfig> {
    Ok(SweepConfig {
        tag_keys: string_set([
            "client_count",
            "subscriber_count",
            "publisher_count",
            "worker_count",
            "pending_count",
            "route_count",
            "queue_count",
            "area_count",
            "family_count",
            "payload_size",
            "message_size",
            "fanout_size",
            "batch_size",
            "list_size",
            "depth",
        ]),
        tag_key_suffixes: string_vec(["_count", "_size", "_depth"]),
        context_tag_keys: string_vec([
            "measurement_scope",
            "match_kind",
            "ready_state",
            "operation",
            "layer",
        ]),
        name_patterns: vec![
            sweep_pattern(
                "concurrency",
                r"(?P<prefix>scaling|concurrency|clients?)_(?P<value>\d+[a-zA-Z]*)",
            )?,
            sweep_pattern(
                "subscriber_count",
                r"(?P<prefix>subscribers?|subscriber_count)_(?P<value>\d+[a-zA-Z]*)",
            )?,
            sweep_pattern(
                "payload_size",
                r"(?P<prefix>payload|message|msg)_(?P<value>\d+[a-zA-Z]*)",
            )?,
            sweep_pattern(
                "route_depth",
                r"(?P<prefix>depth)_(?P<value>\d+[a-zA-Z]*)",
            )?,
            sweep_pattern(
                "fanout_size",
                r"(?P<prefix>fanout)_(?P<value>\d+[a-zA-Z]*)",
            )?,
            sweep_pattern(
                "batch_size",
                r"(?P<prefix>batch|batch_size)_(?P<value>\d+[a-zA-Z]*)",
            )?,
            sweep_pattern("list_size", r"(?P<prefix>list)_(?P<value>\d+[a-zA-Z]*)")?,
        ],
    })
}

fn sweep_pattern(parameter: &str, regex: &str) -> Result<SweepNamePatternConfig> {
    Ok(SweepNamePatternConfig {
        parameter: parameter.to_string(),
        regex: Regex::new(regex)?,
        value_capture: "value".to_string(),
        prefix_capture: Some("prefix".to_string()),
    })
}

fn string_set<const N: usize>(values: [&str; N]) -> BTreeSet<String> {
    values.into_iter().map(str::to_string).collect()
}

fn string_vec<const N: usize>(values: [&str; N]) -> Vec<String> {
    values.into_iter().map(str::to_string).collect()
}
