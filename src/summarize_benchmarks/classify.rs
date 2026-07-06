use super::compare::ComparisonOutcome;
use super::config::{BenchSummaryConfig, StabilityThresholds};
use super::model::{BenchmarkRecord, ComparisonSummary};

pub fn classify_stability(rel_stddev: Option<f64>, thresholds: &StabilityThresholds) -> String {
    match rel_stddev {
        None => "unknown".to_string(),
        Some(value) if value <= thresholds.stable => "stable".to_string(),
        Some(value) if value <= thresholds.acceptable => "acceptable".to_string(),
        Some(value) if value <= thresholds.noisy => "noisy".to_string(),
        Some(_) => "untrustworthy".to_string(),
    }
}

pub fn current_run_authoritative(records: &[BenchmarkRecord], config: &BenchSummaryConfig) -> bool {
    !records.is_empty()
        && records.iter().all(|record| {
            is_authoritative_status(record.status.as_deref(), config)
                && is_authoritative_stability(record.stability.as_deref(), config)
        })
}

pub fn authority_label(
    records: &[BenchmarkRecord],
    summary: &ComparisonSummary,
    config: &BenchSummaryConfig,
) -> &'static str {
    if records.is_empty() {
        return "not trustworthy enough for authority";
    }
    if default_profile_accepted(records, config) {
        return "accepted default profile, not lab-authoritative";
    }
    if summary.authoritative {
        return "lab-authoritative";
    }
    "not trustworthy enough for authority"
}

pub fn build_comparison_summary(
    records: &[BenchmarkRecord],
    comparison: &ComparisonOutcome,
    baseline_available: bool,
    risk_areas: usize,
    config: &BenchSummaryConfig,
) -> ComparisonSummary {
    ComparisonSummary {
        performance_changes: comparison
            .deltas
            .iter()
            .filter(|item| item.delta_pct.is_some())
            .count(),
        improved: comparison
            .deltas
            .iter()
            .filter(|item| {
                item.directional_delta_pct.unwrap_or(0.0)
                    >= config.thresholds.warning_regression_pct
            })
            .count(),
        regressions: comparison
            .deltas
            .iter()
            .filter(|item| {
                item.directional_delta_pct.unwrap_or(0.0)
                    <= -config.thresholds.warning_regression_pct
            })
            .count(),
        critical: comparison
            .deltas
            .iter()
            .filter(|item| {
                item.directional_delta_pct.unwrap_or(0.0)
                    <= -config.thresholds.critical_regression_pct
            })
            .count(),
        stability_regressions: comparison
            .deltas
            .iter()
            .filter(|item| {
                is_authoritative_stability(item.baseline_stability.as_deref(), config)
                    && !is_authoritative_stability(item.current_stability.as_deref(), config)
            })
            .count(),
        stability_improvements: comparison
            .deltas
            .iter()
            .filter(|item| {
                !is_authoritative_stability(item.baseline_stability.as_deref(), config)
                    && is_authoritative_stability(item.current_stability.as_deref(), config)
            })
            .count(),
        new: comparison.new_records.len(),
        missing: comparison.missing_records.len(),
        risk_areas,
        authoritative: current_run_authoritative(records, config),
        baseline_available,
    }
}

pub fn collect_measurement_notes(
    records: &[BenchmarkRecord],
    comparison: &ComparisonOutcome,
    config: &BenchSummaryConfig,
) -> Vec<String> {
    let mut notes = Vec::new();

    for record in records
        .iter()
        .filter(|record| record.status.as_deref() == Some("insufficient_data"))
        .take(12)
    {
        notes.push(format!(
            "{}: insufficient data ({} sample(s)).",
            record_label(record),
            record.samples.unwrap_or_default()
        ));
    }

    let mut unstable_records: Vec<&BenchmarkRecord> = records
        .iter()
        .filter(|record| matches!(record.stability.as_deref(), Some("noisy" | "untrustworthy")))
        .collect();
    unstable_records.sort_by(|left, right| {
        right
            .rel_stddev
            .unwrap_or_default()
            .partial_cmp(&left.rel_stddev.unwrap_or_default())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for record in unstable_records.into_iter().take(8) {
        notes.push(format!(
            "{}: unstable variance ({} RSD).",
            record_label(record),
            format_rel_stddev(record.rel_stddev)
        ));
    }

    for record in records
        .iter()
        .filter(|record| {
            record.status.as_deref().is_some_and(|status| {
                status != "insufficient_data" && !config.authoritative_statuses.contains(status)
            })
        })
        .take(8)
    {
        notes.push(format!(
            "{}: status {} requires manual review.",
            record_label(record),
            record.status.as_deref().unwrap_or("unknown")
        ));
    }

    let mut critical_regressions = comparison
        .deltas
        .iter()
        .filter(|item| {
            item.directional_delta_pct.unwrap_or(0.0) <= -config.thresholds.critical_regression_pct
        })
        .collect::<Vec<_>>();
    critical_regressions.sort_by(|left, right| {
        left.directional_delta_pct
            .unwrap_or_default()
            .partial_cmp(&right.directional_delta_pct.unwrap_or_default())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for delta in critical_regressions.into_iter().take(8) {
        notes.push(format!(
            "{} / {} / {}: critical regression {} vs baseline.",
            delta.suite,
            delta.case,
            delta.metric,
            format_delta(delta.directional_delta_pct)
        ));
    }

    push_semantic_change_notes(&mut notes, comparison);

    if !comparison.new_records.is_empty() {
        notes.push(format!(
            "{} current record(s) do not have baseline coverage.",
            comparison.new_records.len()
        ));
    }
    if !comparison.missing_records.is_empty() {
        notes.push(format!(
            "{} baseline record(s) were not present in the current run.",
            comparison.missing_records.len()
        ));
    }
    push_probable_rename_notes(&mut notes, comparison);

    if notes.is_empty() {
        notes.push("No major measurement warnings detected in current data.".to_string());
    }

    notes
}

fn push_semantic_change_notes(notes: &mut Vec<String>, comparison: &ComparisonOutcome) {
    for delta in comparison
        .deltas
        .iter()
        .filter(|delta| delta.semantic_change.is_some())
        .take(8)
    {
        notes.push(format!(
            "{} / {} / {}: {}; baseline should be refreshed.",
            delta.suite,
            delta.case,
            delta.metric,
            delta
                .semantic_change
                .as_deref()
                .unwrap_or("semantic change")
        ));
    }
}

fn push_probable_rename_notes(notes: &mut Vec<String>, comparison: &ComparisonOutcome) {
    for rename in comparison.probable_renames.iter().take(8) {
        notes.push(format!(
            "Probable renamed row: {} -> {} ({}: {} -> {}).",
            rename.baseline_id,
            rename.current_id,
            rename.changed_key,
            rename.baseline_value,
            rename.current_value
        ));
    }
}

fn is_authoritative_status(status: Option<&str>, config: &BenchSummaryConfig) -> bool {
    status.is_some_and(|value| config.authoritative_statuses.contains(value))
}

fn is_authoritative_stability(stability: Option<&str>, config: &BenchSummaryConfig) -> bool {
    stability.is_some_and(|value| config.authoritative_stabilities.contains(value))
}

fn default_profile_accepted(records: &[BenchmarkRecord], config: &BenchSummaryConfig) -> bool {
    records.iter().all(|record| {
        record
            .metadata
            .get("run_profile")
            .is_some_and(|value| value == "default")
            && is_authoritative_status(record.status.as_deref(), config)
            && is_authoritative_stability(record.stability.as_deref(), config)
    })
}

fn record_label(record: &BenchmarkRecord) -> String {
    match &record.scenario {
        Some(scenario) => format!(
            "{} / {} / {} ({scenario})",
            record.adapter, record.suite, record.case
        ),
        None => format!("{} / {} / {}", record.adapter, record.suite, record.case),
    }
}

fn format_rel_stddev(value: Option<f64>) -> String {
    value.map_or_else(
        || "NA".to_string(),
        |value| format!("{:.1}%", value * 100.0),
    )
}

fn format_delta(value: Option<f64>) -> String {
    match value {
        None => "NA".to_string(),
        Some(value) => {
            let sign = if value >= 0.0 { "+" } else { "" };
            format!("{sign}{value:.1}%")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::config::BenchSummaryConfig;
    use crate::model::{BenchmarkRecord, MetricDirection};

    fn record(status: &str, stability: &str) -> BenchmarkRecord {
        BenchmarkRecord {
            id: "id".to_string(),
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
            stability: Some(stability.to_string()),
            status: Some(status.to_string()),
            rel_stddev: Some(0.01),
            tags: BTreeMap::new(),
            metadata: BTreeMap::new(),
            source_file: "target/file".to_string(),
        }
    }

    #[test]
    fn should_classify_run_authoritative_when_all_records_have_authoritative_status_and_stability()
    {
        // Arrange
        let config = BenchSummaryConfig::for_tests();
        let records = vec![record("authoritative", "stable")];

        // Act
        let authoritative = current_run_authoritative(&records, &config);

        // Assert
        assert!(authoritative);
    }

    #[test]
    fn should_not_classify_run_authoritative_when_any_record_is_not_authoritative() {
        // Arrange
        let config = BenchSummaryConfig::for_tests();
        let records = vec![
            record("authoritative", "stable"),
            record("authoritative", "noisy"),
        ];

        // Act
        let authoritative = current_run_authoritative(&records, &config);

        // Assert
        assert!(!authoritative);
    }

    #[test]
    fn should_label_default_profile_accepted_rows_without_lab_authority() {
        // Arrange
        let config = BenchSummaryConfig::for_tests();
        let mut current = record("acceptable", "stable");
        current
            .metadata
            .insert("run_profile".to_string(), "default".to_string());
        let summary = ComparisonSummary {
            authoritative: true,
            ..ComparisonSummary::default()
        };

        // Act
        let label = authority_label(&[current], &summary, &config);

        // Assert
        assert_eq!(label, "accepted default profile, not lab-authoritative");
    }

    #[test]
    fn should_label_authoritative_non_default_rows_as_lab_authoritative() {
        // Arrange
        let config = BenchSummaryConfig::for_tests();
        let summary = ComparisonSummary {
            authoritative: true,
            ..ComparisonSummary::default()
        };

        // Act
        let label = authority_label(&[record("authoritative", "stable")], &summary, &config);

        // Assert
        assert_eq!(label, "lab-authoritative");
    }

    #[test]
    fn should_label_noisy_rows_as_not_trustworthy_for_authority() {
        // Arrange
        let config = BenchSummaryConfig::for_tests();
        let summary = ComparisonSummary::default();

        // Act
        let label = authority_label(&[record("authoritative", "noisy")], &summary, &config);

        // Assert
        assert_eq!(label, "not trustworthy enough for authority");
    }
}
