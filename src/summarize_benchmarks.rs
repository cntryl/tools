use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use chrono::Utc;
use clap::Args;

use crate::adapters::enabled_adapters;
use crate::classify::{
    authority_label, build_comparison_summary, collect_measurement_notes, current_run_authoritative,
};
use crate::compare::compare_records;
use crate::config::BenchSummaryConfig;
use crate::model::{
    BenchmarkManifest, BenchmarkRecord, ComparisonSummary, BENCHMARK_MANIFEST_SCHEMA_VERSION,
};
use crate::report::{write_adapter_csv, write_markdown_report};
use crate::sweep::detect_sweep_groups;

#[derive(Debug, Args, Clone)]
pub struct SummarizeBenchmarksArgs {
    #[arg(long, default_value = ".")]
    pub root: PathBuf,
    #[arg(long, default_value = "cntryl")]
    pub product_name: String,
    #[arg(long, default_value = "Cntryl Benchmark Report")]
    pub report_title: String,
}

pub fn run(args: SummarizeBenchmarksArgs) -> Result<i32> {
    let config = BenchSummaryConfig::for_root(&args.root, args.product_name, args.report_title)?;
    run_with_config(&config)
}

fn run_with_config(config: &BenchSummaryConfig) -> Result<i32> {
    let mut records = collect_records(config)?;
    records.retain(|record| !config.should_ignore_record(record));
    sort_records(&mut records);

    write_adapter_csv_artifacts(config, &records)?;

    let baseline = load_baseline_manifest(&config.baseline_file)?;
    let comparison = compare_records(&records, baseline.as_ref());
    let notes = collect_measurement_notes(&records, &comparison, config);
    let summary = build_comparison_summary(
        &records,
        &comparison,
        baseline.is_some(),
        notes.len(),
        config,
    );
    let manifest = BenchmarkManifest {
        schema_version: BENCHMARK_MANIFEST_SCHEMA_VERSION,
        generated_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        commit_hash: git_commit_hash(&config.root),
        comparison_summary: summary.clone(),
        records: records.clone(),
    };
    let sweeps = detect_sweep_groups(&records, config);

    write_json(&config.manifest_file, &manifest)?;
    write_markdown_report(config, &manifest, &comparison, &sweeps, &notes)?;

    println!("Wrote {} (manifest).", config.manifest_file.display());
    println!("Wrote {} (report).", config.markdown_report_file.display());
    print_comparison_counters(&records, &summary, config);

    if current_run_authoritative(&records, config)
        && summary.critical == 0
        && summary.new == 0
        && summary.missing == 0
    {
        write_json(&config.baseline_file, &manifest)?;
        println!("Promoted {} (baseline).", config.baseline_file.display());
    }

    Ok(summary_exit_code(&summary))
}

fn print_comparison_counters(
    records: &[BenchmarkRecord],
    summary: &ComparisonSummary,
    config: &BenchSummaryConfig,
) {
    println!(
        "Comparison counters: critical={} new={} missing={} regressions={} improvements={} authority={}",
        summary.critical,
        summary.new,
        summary.missing,
        summary.regressions,
        summary.improved,
        authority_label(records, summary, config)
    );
}

fn summary_exit_code(summary: &ComparisonSummary) -> i32 {
    i32::from(summary.critical > 0 || summary.new > 0 || summary.missing > 0)
}

fn collect_records(config: &BenchSummaryConfig) -> Result<Vec<BenchmarkRecord>> {
    let mut records = Vec::new();
    for adapter in enabled_adapters(config) {
        records.extend(adapter.collect(config)?);
    }
    Ok(records)
}

fn sort_records(records: &mut [BenchmarkRecord]) {
    records.sort_by(|left, right| {
        left.adapter
            .cmp(&right.adapter)
            .then_with(|| left.suite.cmp(&right.suite))
            .then_with(|| left.case.cmp(&right.case))
            .then_with(|| left.scenario.cmp(&right.scenario))
            .then_with(|| left.metric.cmp(&right.metric))
            .then_with(|| left.id.cmp(&right.id))
    });
}

fn write_adapter_csv_artifacts(
    config: &BenchSummaryConfig,
    records: &[BenchmarkRecord],
) -> Result<()> {
    if let Some(path) = config.criterion_csv_output() {
        let adapter_records: Vec<_> = records
            .iter()
            .filter(|record| record.adapter == "criterion")
            .cloned()
            .collect();
        if !adapter_records.is_empty() {
            write_adapter_csv(path, &adapter_records)?;
            println!(
                "Wrote {} (criterion) with {} entries.",
                path.display(),
                adapter_records.len()
            );
        }
    }

    if let Some(path) = config.stress_csv_output() {
        let adapter_records: Vec<_> = records
            .iter()
            .filter(|record| record.adapter == "stress")
            .cloned()
            .collect();
        if !adapter_records.is_empty() {
            write_adapter_csv(path, &adapter_records)?;
            println!(
                "Wrote {} (stress) with {} entries.",
                path.display(),
                adapter_records.len()
            );
        }
    }

    Ok(())
}

fn load_baseline_manifest(path: &Path) -> Result<Option<BenchmarkManifest>> {
    if !path.exists() {
        return Ok(None);
    }

    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read baseline {}", path.display()))?;

    let manifest: BenchmarkManifest = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse baseline {}", path.display()))?;
    if manifest.schema_version != BENCHMARK_MANIFEST_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported baseline schema_version {} in {}; expected {}",
            manifest.schema_version,
            path.display(),
            BENCHMARK_MANIFEST_SCHEMA_VERSION
        );
    }

    Ok(Some(manifest))
}

fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let content = serde_json::to_string_pretty(value)?;
    fs::write(path, format!("{content}\n"))
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn git_commit_hash(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("HEAD")
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MetricDirection;
    use std::collections::BTreeMap;

    fn record(id: &str) -> BenchmarkRecord {
        BenchmarkRecord {
            id: id.to_string(),
            adapter: "criterion".to_string(),
            suite: "suite".to_string(),
            case: "case".to_string(),
            scenario: None,
            metric: "latency_ns".to_string(),
            unit: "ns".to_string(),
            value: 1.0,
            lower_bound: None,
            upper_bound: None,
            samples: None,
            metric_direction: MetricDirection::LowerIsBetter,
            stability: Some("stable".to_string()),
            status: Some("authoritative".to_string()),
            rel_stddev: Some(0.01),
            tags: BTreeMap::new(),
            metadata: BTreeMap::new(),
            source_file: "target/file".to_string(),
        }
    }

    #[test]
    fn should_filter_record_when_exact_id_is_ignored() {
        // Arrange
        let config = BenchSummaryConfig::for_tests_with_exact_ignore(["stale-id"]);

        // Act
        let ignored = config.should_ignore_record(&record("stale-id"));

        // Assert
        assert!(ignored);
    }

    #[test]
    fn should_filter_record_when_pattern_matches_metadata() {
        // Arrange
        let mut config = BenchSummaryConfig::for_tests();
        config.ignored_patterns =
            vec![regex::Regex::new("schedule_system_scan_and_fire").expect("valid ignore regex")];
        let mut record = record("record-id");
        record.metadata.insert(
            "raw_benchmark".to_string(),
            "schedule_system_scan_and_fire/case".to_string(),
        );

        // Act
        let ignored = config.should_ignore_record(&record);

        // Assert
        assert!(ignored);
    }

    #[test]
    fn should_exit_nonzero_for_critical_new_or_missing_records() {
        // Arrange
        let failing_summaries = [
            ComparisonSummary {
                critical: 1,
                ..ComparisonSummary::default()
            },
            ComparisonSummary {
                new: 1,
                ..ComparisonSummary::default()
            },
            ComparisonSummary {
                missing: 1,
                ..ComparisonSummary::default()
            },
        ];

        // Act
        let failing_exit_codes = failing_summaries.map(|summary| summary_exit_code(&summary));
        let clean_exit_code = summary_exit_code(&ComparisonSummary::default());

        // Assert
        assert_eq!(failing_exit_codes, [1, 1, 1]);
        assert_eq!(clean_exit_code, 0);
    }
}
