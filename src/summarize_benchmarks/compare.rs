use std::collections::{BTreeMap, BTreeSet};

use super::model::{BenchmarkDelta, BenchmarkManifest, BenchmarkRecord};

#[derive(Debug, Clone, Default)]
pub struct ComparisonOutcome {
    pub deltas: Vec<BenchmarkDelta>,
    pub new_records: Vec<BenchmarkRecord>,
    pub missing_records: Vec<BenchmarkRecord>,
    pub probable_renames: Vec<ProbableRename>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbableRename {
    pub baseline_id: String,
    pub current_id: String,
    pub changed_key: String,
    pub baseline_value: String,
    pub current_value: String,
}

pub fn compare_records(
    current: &[BenchmarkRecord],
    baseline: Option<&BenchmarkManifest>,
) -> ComparisonOutcome {
    let mut baseline_map = BTreeMap::new();
    if let Some(baseline) = baseline {
        for record in &baseline.records {
            baseline_map.insert(record.id.clone(), record.clone());
        }
    }

    let mut deltas = Vec::with_capacity(current.len());
    let mut new_records = Vec::new();
    let mut current_ids = BTreeSet::new();

    for record in current {
        current_ids.insert(record.id.clone());

        let baseline_record = baseline_map.get(&record.id);
        let baseline_value = baseline_record
            .map(|value| value.value)
            .filter(|value| value.is_finite() && *value > 0.0);
        let semantic_change =
            baseline_record.and_then(|baseline| semantic_change_reason(record, baseline));
        let delta_pct = if semantic_change.is_some() {
            None
        } else {
            baseline_value.map(|value| ((record.value - value) / value) * 100.0)
        };

        if baseline_record.is_none() {
            new_records.push(record.clone());
        }

        deltas.push(BenchmarkDelta {
            id: record.id.clone(),
            suite: record.suite.clone(),
            case: record.case.clone(),
            metric: record.metric.clone(),
            baseline_value,
            current_value: record.value,
            delta_pct,
            directional_delta_pct: delta_pct
                .map(|value| record.metric_direction.directional_delta(value)),
            baseline_stability: baseline_record.and_then(|value| value.stability.clone()),
            current_stability: record.stability.clone(),
            baseline_status: baseline_record.and_then(|value| value.status.clone()),
            current_status: record.status.clone(),
            semantic_change,
        });
    }

    deltas.sort_by(|left, right| {
        left.suite
            .cmp(&right.suite)
            .then_with(|| left.case.cmp(&right.case))
            .then_with(|| left.metric.cmp(&right.metric))
            .then_with(|| left.id.cmp(&right.id))
    });

    let mut missing_records = baseline_map
        .into_iter()
        .filter(|(id, _)| !current_ids.contains(id))
        .map(|(_, record)| record)
        .collect::<Vec<_>>();
    missing_records.sort_by(|left, right| {
        left.adapter
            .cmp(&right.adapter)
            .then_with(|| left.suite.cmp(&right.suite))
            .then_with(|| left.case.cmp(&right.case))
            .then_with(|| left.metric.cmp(&right.metric))
            .then_with(|| left.id.cmp(&right.id))
    });

    let probable_renames = probable_renames(&comparison_pairs(&new_records, &missing_records));

    ComparisonOutcome {
        deltas,
        new_records,
        missing_records,
        probable_renames,
    }
}

fn semantic_change_reason(current: &BenchmarkRecord, baseline: &BenchmarkRecord) -> Option<String> {
    let current_basis = current.metadata.get("ns_per_op_basis");
    let baseline_basis = baseline.metadata.get("ns_per_op_basis");
    (current_basis != baseline_basis).then(|| {
        format!(
            "ns_per_op_basis changed from {} to {}; refresh the baseline after confirming the semantic change is intentional",
            baseline_basis.map_or("<unset>", String::as_str),
            current_basis.map_or("<unset>", String::as_str)
        )
    })
}

fn comparison_pairs<'a>(
    new_records: &'a [BenchmarkRecord],
    missing_records: &'a [BenchmarkRecord],
) -> Vec<(&'a BenchmarkRecord, &'a BenchmarkRecord)> {
    new_records
        .iter()
        .flat_map(|current| {
            missing_records
                .iter()
                .map(move |baseline| (current, baseline))
        })
        .collect()
}

fn probable_renames(pairs: &[(&BenchmarkRecord, &BenchmarkRecord)]) -> Vec<ProbableRename> {
    let mut renames = pairs
        .iter()
        .filter_map(|(current, baseline)| probable_rename(current, baseline))
        .collect::<Vec<_>>();
    renames.sort_by(|left, right| {
        left.baseline_id
            .cmp(&right.baseline_id)
            .then_with(|| left.current_id.cmp(&right.current_id))
    });
    renames
}

fn probable_rename(
    current: &BenchmarkRecord,
    baseline: &BenchmarkRecord,
) -> Option<ProbableRename> {
    if current.adapter != baseline.adapter || current.metric != baseline.metric {
        return None;
    }
    one_changed_key_value_segment(&baseline.id, &current.id).map(
        |(changed_key, baseline_value, current_value)| ProbableRename {
            baseline_id: baseline.id.clone(),
            current_id: current.id.clone(),
            changed_key,
            baseline_value,
            current_value,
        },
    )
}

fn one_changed_key_value_segment(
    baseline_id: &str,
    current_id: &str,
) -> Option<(String, String, String)> {
    let baseline = baseline_id.split('|').collect::<Vec<_>>();
    let current = current_id.split('|').collect::<Vec<_>>();
    if baseline.len() != current.len() {
        return None;
    }
    let changed = baseline
        .iter()
        .zip(current.iter())
        .filter(|(left, right)| left != right)
        .collect::<Vec<_>>();
    let [(baseline_segment, current_segment)] = changed.as_slice() else {
        return None;
    };
    let (baseline_key, baseline_value) = baseline_segment.split_once('=')?;
    let (current_key, current_value) = current_segment.split_once('=')?;
    (baseline_key == current_key && baseline_value != current_value).then(|| {
        (
            baseline_key.to_string(),
            baseline_value.to_string(),
            current_value.to_string(),
        )
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::model::{
        BenchmarkManifest, ComparisonSummary, MetricDirection, BENCHMARK_MANIFEST_SCHEMA_VERSION,
    };

    fn record(id: &str, case: &str, direction: MetricDirection, value: f64) -> BenchmarkRecord {
        BenchmarkRecord {
            id: id.to_string(),
            adapter: "criterion".to_string(),
            suite: "suite".to_string(),
            case: case.to_string(),
            scenario: None,
            metric: match direction {
                MetricDirection::LowerIsBetter => "latency_ns".to_string(),
                MetricDirection::HigherIsBetter => "throughput_ops_per_s".to_string(),
            },
            unit: match direction {
                MetricDirection::LowerIsBetter => "ns".to_string(),
                MetricDirection::HigherIsBetter => "ops/s".to_string(),
            },
            value,
            lower_bound: None,
            upper_bound: None,
            samples: None,
            metric_direction: direction,
            stability: Some("stable".to_string()),
            status: Some("authoritative".to_string()),
            rel_stddev: Some(0.01),
            tags: BTreeMap::new(),
            metadata: BTreeMap::new(),
            source_file: "target/file".to_string(),
        }
    }

    fn manifest(records: Vec<BenchmarkRecord>) -> BenchmarkManifest {
        BenchmarkManifest {
            schema_version: BENCHMARK_MANIFEST_SCHEMA_VERSION,
            generated_at: "now".to_string(),
            commit_hash: None,
            comparison_summary: ComparisonSummary::default(),
            records,
        }
    }

    #[test]
    fn should_compute_positive_directional_delta_when_lower_is_better_metric_improves() {
        // Arrange
        let baseline = manifest(vec![record(
            "stable-id",
            "baseline-case",
            MetricDirection::LowerIsBetter,
            100.0,
        )]);
        let current = vec![record(
            "stable-id",
            "current-case",
            MetricDirection::LowerIsBetter,
            80.0,
        )];

        // Act
        let comparison = compare_records(&current, Some(&baseline));

        // Assert
        assert_eq!(comparison.deltas.len(), 1);
        assert_eq!(comparison.deltas[0].delta_pct, Some(-20.0));
        assert_eq!(comparison.deltas[0].directional_delta_pct, Some(20.0));
    }

    #[test]
    fn should_compute_positive_directional_delta_when_higher_is_better_metric_improves() {
        // Arrange
        let baseline = manifest(vec![record(
            "stable-id",
            "baseline-case",
            MetricDirection::HigherIsBetter,
            100.0,
        )]);
        let current = vec![record(
            "stable-id",
            "current-case",
            MetricDirection::HigherIsBetter,
            120.0,
        )];

        // Act
        let comparison = compare_records(&current, Some(&baseline));

        // Assert
        assert_eq!(comparison.deltas.len(), 1);
        assert_eq!(comparison.deltas[0].delta_pct, Some(20.0));
        assert_eq!(comparison.deltas[0].directional_delta_pct, Some(20.0));
    }

    #[test]
    fn should_match_baseline_by_stable_id_given_current_case_label_changes() {
        // Arrange
        let baseline = manifest(vec![record(
            "stable-id",
            "old-case",
            MetricDirection::LowerIsBetter,
            100.0,
        )]);
        let current = vec![record(
            "stable-id",
            "renamed-case",
            MetricDirection::LowerIsBetter,
            95.0,
        )];

        // Act
        let comparison = compare_records(&current, Some(&baseline));

        // Assert
        assert_eq!(comparison.deltas[0].baseline_value, Some(100.0));
        assert_eq!(comparison.deltas[0].case, "renamed-case");
    }

    #[test]
    fn should_treat_ns_per_op_basis_mismatch_as_semantic_change() {
        // Arrange
        let mut baseline_record =
            record("stable-id", "case", MetricDirection::LowerIsBetter, 100.0);
        baseline_record.metric = "ns_per_op".to_string();
        let mut current_record = record("stable-id", "case", MetricDirection::LowerIsBetter, 80.0);
        current_record.metric = "ns_per_op".to_string();
        current_record.metadata.insert(
            "ns_per_op_basis".to_string(),
            "logical_completed_operation".to_string(),
        );
        let baseline = manifest(vec![baseline_record]);

        // Act
        let comparison = compare_records(&[current_record], Some(&baseline));

        // Assert
        assert_eq!(comparison.deltas[0].delta_pct, None);
        assert_eq!(comparison.deltas[0].directional_delta_pct, None);
        assert!(comparison.deltas[0]
            .semantic_change
            .as_deref()
            .is_some_and(|reason| reason.contains("ns_per_op_basis changed")));
    }

    #[test]
    fn should_suggest_probable_renamed_rows_without_matching_them() {
        // Arrange
        let baseline = manifest(vec![record(
            "suite/bench|throughput_ops_per_s|completed_unit=message",
            "case",
            MetricDirection::HigherIsBetter,
            100.0,
        )]);
        let current = vec![record(
            "suite/bench|throughput_ops_per_s|completed_unit=event",
            "case",
            MetricDirection::HigherIsBetter,
            100.0,
        )];

        // Act
        let comparison = compare_records(&current, Some(&baseline));

        // Assert
        assert_eq!(comparison.new_records.len(), 1);
        assert_eq!(comparison.missing_records.len(), 1);
        assert_eq!(comparison.probable_renames.len(), 1);
        assert_eq!(comparison.probable_renames[0].changed_key, "completed_unit");
        assert_eq!(comparison.probable_renames[0].baseline_value, "message");
        assert_eq!(comparison.probable_renames[0].current_value, "event");
    }
}
