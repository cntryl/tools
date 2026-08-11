use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as FmtWrite;
use std::fs;
use std::io::{self, Write as IoWrite};
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{json, Value};

use super::model::{
    AnalysisGap, AppliedExemption, Finding, FindingCategory, OperationalError, ReportStatus,
    SourceLocation, ValidationReport,
};

const SARIF_SCHEMA: &str =
    "https://docs.oasis-open.org/sarif/sarif/v2.1.0/errata01/os/schemas/sarif-schema-2.1.0.json";
const SARIF_VERSION: &str = "2.1.0";
const SARIF_FINGERPRINT_NAME: &str = "cntrylTestValidation/v1";
const EXEMPTION_NOTIFICATION: &str = "cntryl.exemption.applied";

/// Renders every finding and its remediation evidence in a deterministic order.
pub fn render_human(report: &ValidationReport) -> String {
    let report = normalized_report(report);
    let mut output = String::new();

    render_operational_errors(&mut output, &report.errors);
    render_findings(&mut output, &report.findings);
    render_analysis_gaps(&mut output, &report.analysis_gaps);
    render_exemptions(&mut output, &report.exemptions);
    render_summary(&mut output, &report);

    output
}

/// Prints findings and warnings to stdout and operational diagnostics to stderr.
pub fn print_human(report: &ValidationReport) -> Result<()> {
    if report.errors.is_empty() {
        return write_stdout(&render_human(report));
    }

    let report = normalized_report(report);
    let mut standard_output = String::new();
    render_findings(&mut standard_output, &report.findings);
    render_analysis_gaps(&mut standard_output, &report.analysis_gaps);
    render_exemptions(&mut standard_output, &report.exemptions);
    if !standard_output.is_empty() {
        write_stdout(&standard_output)?;
    }

    let mut diagnostic_output = String::new();
    render_operational_errors(&mut diagnostic_output, &report.errors);
    render_summary(&mut diagnostic_output, &report);
    let stderr = io::stderr();
    stderr
        .lock()
        .write_all(diagnostic_output.as_bytes())
        .context("failed to write test validation operational diagnostics")
}

/// Writes the versioned native report with stable ordering and a trailing newline.
pub fn write_json(path: &Path, report: &ValidationReport) -> Result<()> {
    let report = normalized_report(report);
    write_pretty_json(path, &report, "JSON")
}

/// Writes a SARIF 2.1.0 report suitable for CI result ingestion.
pub fn write_sarif(path: &Path, report: &ValidationReport) -> Result<()> {
    let report = normalized_report(report);
    let sarif = sarif_value(&report);
    write_pretty_json(path, &sarif, "SARIF")
}

/// Returns the validator process exit code represented by a report.
pub fn exit_code(report: &ValidationReport) -> i32 {
    if report.status == ReportStatus::Incomplete || !report.errors.is_empty() {
        2
    } else {
        i32::from(!report.findings.is_empty())
    }
}

fn write_pretty_json<T: Serialize + ?Sized>(
    path: &Path,
    value: &T,
    format_name: &str,
) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let mut content = serde_json::to_vec_pretty(value)
        .with_context(|| format!("failed to serialize {format_name} report"))?;
    content.push(b'\n');
    fs::write(path, content)
        .with_context(|| format!("failed to write {format_name} report to {}", path.display()))
}

fn normalized_report(report: &ValidationReport) -> ValidationReport {
    let mut report = report.clone();
    report.root = normalize_path(&report.root);

    for finding in &mut report.findings {
        normalize_location(&mut finding.primary);
        for evidence in &mut finding.evidence {
            normalize_location(&mut evidence.location);
        }
    }
    for gap in &mut report.analysis_gaps {
        normalize_location(&mut gap.location);
    }
    for error in &mut report.errors {
        if let Some(location) = &mut error.location {
            normalize_location(location);
        }
    }

    report.findings.sort_by(|left, right| {
        left.primary
            .path
            .cmp(&right.primary.path)
            .then_with(|| left.test.qualified_name.cmp(&right.test.qualified_name))
            .then_with(|| left.rule_id.cmp(&right.rule_id))
            .then_with(|| compare_spans(&left.primary, &right.primary))
    });
    report.analysis_gaps.sort_by(|left, right| {
        left.location
            .path
            .cmp(&right.location.path)
            .then_with(|| left.test.qualified_name.cmp(&right.test.qualified_name))
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| compare_spans(&left.location, &right.location))
    });
    report.exemptions.sort_by(|left, right| {
        left.test
            .qualified_name
            .cmp(&right.test.qualified_name)
            .then_with(|| left.rule_id.cmp(&right.rule_id))
            .then_with(|| left.fingerprint.cmp(&right.fingerprint))
    });
    report.errors.sort_by(|left, right| {
        left.code
            .cmp(&right.code)
            .then_with(|| {
                compare_optional_locations(left.location.as_ref(), right.location.as_ref())
            })
            .then_with(|| left.message.cmp(&right.message))
    });
    report
}

fn write_stdout(content: &str) -> Result<()> {
    let stdout = io::stdout();
    stdout
        .lock()
        .write_all(content.as_bytes())
        .context("failed to write test validation diagnostics")
}

fn normalize_location(location: &mut SourceLocation) {
    location.path = normalize_path(&location.path);
}

fn normalize_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    normalized
        .strip_prefix("./")
        .unwrap_or(&normalized)
        .to_string()
}

fn compare_locations(left: &SourceLocation, right: &SourceLocation) -> std::cmp::Ordering {
    left.path
        .cmp(&right.path)
        .then_with(|| compare_spans(left, right))
}

fn compare_spans(left: &SourceLocation, right: &SourceLocation) -> std::cmp::Ordering {
    left.span
        .start
        .line
        .cmp(&right.span.start.line)
        .then_with(|| left.span.start.column.cmp(&right.span.start.column))
        .then_with(|| left.span.end.line.cmp(&right.span.end.line))
        .then_with(|| left.span.end.column.cmp(&right.span.end.column))
}

fn compare_optional_locations(
    left: Option<&SourceLocation>,
    right: Option<&SourceLocation>,
) -> std::cmp::Ordering {
    match (left, right) {
        (Some(left), Some(right)) => compare_locations(left, right),
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn render_operational_errors(output: &mut String, errors: &[OperationalError]) {
    for error in errors {
        writeln!(output, "error[{}]: {}", error.code, error.message)
            .expect("writing to a String cannot fail");
        if let Some(location) = &error.location {
            writeln!(output, "  --> {}", display_location(location))
                .expect("writing to a String cannot fail");
        }
        output.push('\n');
    }
}

fn render_findings(output: &mut String, findings: &[Finding]) {
    let mut previous_test: Option<(&str, &str)> = None;
    for finding in findings {
        let current_test = (
            finding.primary.path.as_str(),
            finding.test.qualified_name.as_str(),
        );
        if previous_test != Some(current_test) {
            if previous_test.is_some() {
                output.push('\n');
            }
            writeln!(
                output,
                "{}::{}",
                finding.primary.path, finding.test.qualified_name
            )
            .expect("writing to a String cannot fail");
            previous_test = Some(current_test);
        }

        writeln!(output, "error[{}]: {}", finding.rule_id, finding.message)
            .expect("writing to a String cannot fail");
        writeln!(output, "  --> {}", display_location(&finding.primary))
            .expect("writing to a String cannot fail");
        if !finding.evidence.is_empty() {
            output.push_str("  evidence:\n");
            for evidence in &finding.evidence {
                writeln!(
                    output,
                    "    {}: {} - {}",
                    evidence.kind,
                    display_location(&evidence.location),
                    evidence.message
                )
                .expect("writing to a String cannot fail");
            }
        }
        writeln!(output, "  why: {}", finding.why_it_matters)
            .expect("writing to a String cannot fail");
        writeln!(output, "  fix: {}", finding.remediation.summary)
            .expect("writing to a String cannot fail");
        for action in &finding.remediation.actions {
            writeln!(output, "    - {action}").expect("writing to a String cannot fail");
        }
        if let Some(example) = &finding.remediation.example {
            writeln!(output, "  example: {example}").expect("writing to a String cannot fail");
        }
        writeln!(output, "  rerun: {}", finding.remediation.rerun)
            .expect("writing to a String cannot fail");
    }
}

fn render_analysis_gaps(output: &mut String, gaps: &[AnalysisGap]) {
    if gaps.is_empty() {
        return;
    }
    output.push_str("\nAnalysis gaps (non-blocking):\n");
    for gap in gaps {
        writeln!(
            output,
            "  warning[{}] {} at {}: {}",
            gap.code,
            gap.test.qualified_name,
            display_location(&gap.location),
            gap.message
        )
        .expect("writing to a String cannot fail");
    }
}

fn render_exemptions(output: &mut String, exemptions: &[AppliedExemption]) {
    if exemptions.is_empty() {
        return;
    }
    output.push_str("\nApplied exemptions:\n");
    for exemption in exemptions {
        writeln!(
            output,
            "  {} {}: {}",
            exemption.rule_id, exemption.test.qualified_name, exemption.reason
        )
        .expect("writing to a String cannot fail");
    }
}

fn render_summary(output: &mut String, report: &ValidationReport) {
    if !output.is_empty() && !output.ends_with("\n\n") {
        output.push('\n');
    }
    let outcome = if exit_code(report) == 2 {
        "incomplete"
    } else if report.findings.is_empty() {
        "passed"
    } else {
        "failed"
    };
    writeln!(
        output,
        "Validation {outcome}: {} findings affecting {} of {} tests in {} files; {} partially analyzed; {} exemptions applied.",
        report.summary.findings,
        report.summary.tests_with_findings,
        report.summary.total_tests,
        report.summary.total_files,
        report.summary.partially_analyzed_tests,
        report.summary.applied_exemptions
    )
    .expect("writing to a String cannot fail");
}

fn display_location(location: &SourceLocation) -> String {
    let start = location.span.start;
    let end = location.span.end;
    format!(
        "{}:{}:{}-{}:{}",
        location.path, start.line, start.column, end.line, end.column
    )
}

fn sarif_value(report: &ValidationReport) -> Value {
    let rule_findings = first_finding_by_rule(&report.findings);
    let rule_indices: BTreeMap<&str, usize> = rule_findings
        .keys()
        .enumerate()
        .map(|(index, rule_id)| (*rule_id, index))
        .collect();
    let rules: Vec<Value> = rule_findings
        .iter()
        .map(|(rule_id, finding)| sarif_rule(rule_id, finding))
        .collect();

    let notification_ids = notification_ids(report);
    let notification_indices: BTreeMap<&str, usize> = notification_ids
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index))
        .collect();
    let notifications: Vec<Value> = notification_ids
        .iter()
        .map(|id| sarif_notification_descriptor(id))
        .collect();

    let results: Vec<Value> = report
        .findings
        .iter()
        .map(|finding| sarif_result(finding, rule_indices[&finding.rule_id.as_str()]))
        .collect();
    let tool_configuration_notifications =
        configuration_notifications(report, &notification_indices);
    let tool_execution_notifications = execution_notifications(report, &notification_indices);
    let code = exit_code(report);
    let execution_successful = code != 2;
    json!({
        "$schema": SARIF_SCHEMA,
        "version": SARIF_VERSION,
        "runs": [{
            "columnKind": "unicodeCodePoints",
            "defaultEncoding": "utf-8",
            "defaultSourceLanguage": "rust",
            "tool": {
                "driver": {
                    "name": report.tool.name,
                    "semanticVersion": report.tool.version,
                    "rules": rules,
                    "notifications": notifications
                }
            },
            "invocations": [{
                "executionSuccessful": execution_successful,
                "exitCode": code,
                "exitCodeDescription": exit_description(code),
                "toolConfigurationNotifications": tool_configuration_notifications,
                "toolExecutionNotifications": tool_execution_notifications
            }],
            "results": results
        }]
    })
}

fn first_finding_by_rule(findings: &[Finding]) -> BTreeMap<&str, &Finding> {
    let mut by_rule = BTreeMap::new();
    for finding in findings {
        by_rule.entry(finding.rule_id.as_str()).or_insert(finding);
    }
    by_rule
}

fn sarif_rule(rule_id: &str, finding: &Finding) -> Value {
    let category = category_name(finding.category);
    let mut help = finding.remediation.summary.clone();
    if !finding.remediation.actions.is_empty() {
        help.push(' ');
        help.push_str(&finding.remediation.actions.join(" "));
    }
    json!({
        "id": rule_id,
        "shortDescription": { "text": rule_title(rule_id) },
        "fullDescription": { "text": finding.why_it_matters },
        "defaultConfiguration": { "level": sarif_level(finding.severity) },
        "help": { "text": help },
        "properties": {
            "category": category,
            "tags": ["test-quality", category]
        }
    })
}

fn sarif_result(finding: &Finding, rule_index: usize) -> Value {
    let related_locations: Vec<Value> = finding
        .evidence
        .iter()
        .enumerate()
        .map(|(index, evidence)| {
            json!({
                "id": index + 1,
                "message": { "text": format!("{}: {}", evidence.kind, evidence.message) },
                "physicalLocation": sarif_physical_location(&evidence.location)
            })
        })
        .collect();
    let mut message = format!(
        "{}\n\nWhy it matters: {}\n\nRemediation: {}",
        finding.message, finding.why_it_matters, finding.remediation.summary
    );
    if let Some(example) = &finding.remediation.example {
        write!(message, "\n\nExample: {example}").expect("writing to a String cannot fail");
    }
    write!(message, "\n\nRerun: {}", finding.remediation.rerun)
        .expect("writing to a String cannot fail");

    let properties = json!({
        "category": category_name(finding.category),
        "confidence": finding.confidence,
        "testName": finding.test.name,
        "qualifiedTestName": finding.test.qualified_name,
        "whyItMatters": finding.why_it_matters,
        "remediationSummary": finding.remediation.summary,
        "remediationActions": finding.remediation.actions,
        "remediationExample": finding.remediation.example,
        "rerunCommand": finding.remediation.rerun
    });

    json!({
        "ruleId": finding.rule_id,
        "ruleIndex": rule_index,
        "level": sarif_level(finding.severity),
        "message": { "text": message },
        "locations": [{
            "physicalLocation": sarif_physical_location(&finding.primary),
            "logicalLocations": [{
                "name": finding.test.name,
                "fullyQualifiedName": finding.test.qualified_name,
                "kind": "function"
            }]
        }],
        "relatedLocations": related_locations,
        "fingerprints": {
            SARIF_FINGERPRINT_NAME: finding.fingerprint
        },
        "properties": properties
    })
}

fn sarif_physical_location(location: &SourceLocation) -> Value {
    json!({
        "artifactLocation": {
            "uri": uri_path(&location.path)
        },
        "region": {
            "startLine": location.span.start.line,
            "startColumn": location.span.start.column,
            "endLine": location.span.end.line,
            "endColumn": location.span.end.column
        }
    })
}

fn notification_ids(report: &ValidationReport) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    ids.extend(report.analysis_gaps.iter().map(|gap| gap.code.clone()));
    ids.extend(report.errors.iter().map(|error| error.code.clone()));
    if !report.exemptions.is_empty() {
        ids.insert(EXEMPTION_NOTIFICATION.to_string());
    }
    ids
}

fn sarif_notification_descriptor(id: &str) -> Value {
    json!({
        "id": id,
        "shortDescription": { "text": rule_title(id) }
    })
}

fn configuration_notifications(
    report: &ValidationReport,
    indices: &BTreeMap<&str, usize>,
) -> Vec<Value> {
    let mut notifications: Vec<Value> = report
        .errors
        .iter()
        .filter(|error| error.code.starts_with("config."))
        .map(|error| sarif_error_notification(error, indices, "error"))
        .collect();
    notifications.extend(report.exemptions.iter().map(|exemption| {
        json!({
            "descriptor": {
                "id": EXEMPTION_NOTIFICATION,
                "index": indices[EXEMPTION_NOTIFICATION]
            },
            "level": "note",
            "message": {
                "text": format!(
                    "Exempted {} for {}: {}",
                    exemption.rule_id, exemption.test.qualified_name, exemption.reason
                )
            },
            "properties": {
                "ruleId": exemption.rule_id,
                "testName": exemption.test.name,
                "qualifiedTestName": exemption.test.qualified_name,
                "fingerprint": exemption.fingerprint
            }
        })
    }));
    notifications
}

fn execution_notifications(
    report: &ValidationReport,
    indices: &BTreeMap<&str, usize>,
) -> Vec<Value> {
    let mut notifications: Vec<Value> = report
        .errors
        .iter()
        .filter(|error| !error.code.starts_with("config."))
        .map(|error| sarif_error_notification(error, indices, "error"))
        .collect();
    notifications.extend(
        report
            .analysis_gaps
            .iter()
            .map(|gap| sarif_gap_notification(gap, indices)),
    );
    notifications
}

fn sarif_error_notification(
    error: &OperationalError,
    indices: &BTreeMap<&str, usize>,
    level: &str,
) -> Value {
    let locations = error.location.as_ref().map_or_else(Vec::new, |location| {
        vec![json!({ "physicalLocation": sarif_physical_location(location) })]
    });
    json!({
        "descriptor": {
            "id": error.code,
            "index": indices[error.code.as_str()]
        },
        "level": level,
        "message": { "text": error.message },
        "locations": locations
    })
}

fn sarif_gap_notification(gap: &AnalysisGap, indices: &BTreeMap<&str, usize>) -> Value {
    json!({
        "descriptor": {
            "id": gap.code,
            "index": indices[gap.code.as_str()]
        },
        "level": "warning",
        "message": { "text": gap.message },
        "locations": [{
            "physicalLocation": sarif_physical_location(&gap.location),
            "logicalLocations": [{
                "name": gap.test.name,
                "fullyQualifiedName": gap.test.qualified_name,
                "kind": "function"
            }]
        }],
        "properties": {
            "testName": gap.test.name,
            "qualifiedTestName": gap.test.qualified_name
        }
    })
}

fn exit_description(code: i32) -> &'static str {
    match code {
        0 => "Analysis completed with no findings.",
        1 => "Analysis completed with blocking findings.",
        _ => "Analysis was incomplete.",
    }
}

fn category_name(category: FindingCategory) -> &'static str {
    match category {
        FindingCategory::Structure => "structure",
        FindingCategory::Semantics => "semantics",
        FindingCategory::Hygiene => "hygiene",
    }
}

fn sarif_level(severity: &str) -> &'static str {
    match severity {
        "error" => "error",
        "warning" => "warning",
        "note" => "note",
        _ => "none",
    }
}

fn rule_title(rule_id: &str) -> String {
    let title = rule_id.replace(['.', '-'], " ");
    let mut characters = title.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => "Test validation diagnostic".to_string(),
    }
}

fn uri_path(path: &str) -> String {
    let normalized = normalize_path(path);
    let mut uri = String::with_capacity(normalized.len());
    for byte in normalized.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            uri.push(char::from(byte));
        } else {
            write!(uri, "%{byte:02X}").expect("writing to a String cannot fail");
        }
    }
    uri
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::Value;

    use super::{exit_code, render_human, sarif_value, write_json, write_sarif};
    use crate::validate_tests::model::{
        AnalysisGap, Evidence, Finding, FindingCategory, Position, Remediation, ReportStatus,
        SourceLocation, SourceSpan, TestIdentity, ToolInfo, ValidationReport, ValidationSummary,
        REPORT_SCHEMA_VERSION,
    };

    #[test]
    fn should_render_complete_human_remediation() {
        // Arrange
        let report = report_with_finding();

        // Act
        let rendered = render_human(&report);

        // Assert
        assert!(rendered.contains("tests/api.rs::api::should_return_created"));
        assert!(rendered.contains("error[oracle.disconnected]"));
        assert!(rendered.contains("evidence:"));
        assert!(rendered.contains("why: The test can pass"));
        assert!(rendered.contains("fix: Assert the response status."));
        assert!(rendered.contains("example: assert_eq!(response.status(), 201);"));
        assert!(rendered.contains("rerun: cntryl-tools validate-tests -f 'tests/api.rs'"));
        assert!(rendered.contains("Validation failed: 1 findings affecting 1 of 1 tests"));
    }

    #[test]
    fn should_write_sorted_native_json() {
        // Arrange
        let directory = tempfile::tempdir().expect("create report directory");
        let path = directory.path().join("nested/report.json");
        let mut report = report_with_finding();
        let mut later = report.findings[0].clone();
        later.primary.path = ".\\tests\\z.rs".to_string();
        later.test.qualified_name = "z::should_finish".to_string();
        report.findings.insert(0, later);
        report.summary.findings = 2;

        // Act
        write_json(&path, &report).expect("write native report");
        let bytes = fs::read(&path).expect("read native report");
        let value: Value = serde_json::from_slice(&bytes).expect("parse native report");

        // Assert
        assert!(bytes.ends_with(b"\n"));
        assert_eq!(value["schema_version"], REPORT_SCHEMA_VERSION);
        assert_eq!(value["findings"][0]["primary"]["path"], "tests/api.rs");
        assert_eq!(value["findings"][1]["primary"]["path"], "tests/z.rs");
    }

    #[test]
    fn should_map_findings_and_gaps_to_sarif() {
        // Arrange
        let mut report = report_with_finding();
        report.analysis_gaps.push(AnalysisGap {
            code: "analysis.unsupported-macro".to_string(),
            test: identity(),
            message: "custom macro could not be resolved".to_string(),
            location: location("tests/api.rs", 30, 5, 36),
        });
        report.summary.partially_analyzed_tests = 1;

        // Act
        let sarif = sarif_value(&report);
        let run = &sarif["runs"][0];
        let result = &run["results"][0];

        // Assert
        assert_eq!(sarif["version"], "2.1.0");
        assert_eq!(run["columnKind"], "unicodeCodePoints");
        assert_eq!(run["invocations"][0]["exitCode"], 1);
        assert_eq!(run["invocations"][0]["executionSuccessful"], true);
        assert_eq!(result["ruleId"], "oracle.disconnected");
        assert_eq!(
            result["fingerprints"]["cntrylTestValidation/v1"],
            "sha256:0123456789abcdef"
        );
        assert_eq!(
            run["invocations"][0]["toolExecutionNotifications"][0]["level"],
            "warning"
        );
    }

    #[test]
    fn should_write_incomplete_sarif() {
        // Arrange
        let directory = tempfile::tempdir().expect("create report directory");
        let path = directory.path().join("report.sarif");
        let mut report = empty_report();
        report.status = ReportStatus::Incomplete;
        report
            .errors
            .push(crate::validate_tests::model::OperationalError {
                code: "config.invalid".to_string(),
                message: "invalid test policy".to_string(),
                location: None,
            });

        // Act
        write_sarif(&path, &report).expect("write SARIF report");
        let sarif: Value = serde_json::from_slice(&fs::read(&path).expect("read SARIF report"))
            .expect("parse SARIF report");

        // Assert
        assert_eq!(sarif["runs"][0]["results"], Value::Array(Vec::new()));
        assert_eq!(
            sarif["runs"][0]["invocations"][0]["executionSuccessful"],
            false
        );
        assert_eq!(
            sarif["runs"][0]["invocations"][0]["toolConfigurationNotifications"][0]["level"],
            "error"
        );
    }

    #[test]
    fn should_exit_two_for_incomplete_report() {
        // Arrange
        let mut report = empty_report();
        report.status = ReportStatus::Incomplete;

        // Act
        let code = exit_code(&report);

        // Assert
        assert_eq!(code, 2);
    }

    fn report_with_finding() -> ValidationReport {
        let mut report = empty_report();
        report.summary.total_files = 1;
        report.summary.total_tests = 1;
        report.summary.fully_analyzed_tests = 1;
        report.summary.tests_with_findings = 1;
        report.summary.findings = 1;
        report.findings.push(Finding {
            rule_id: "oracle.disconnected".to_string(),
            category: FindingCategory::Semantics,
            severity: "error",
            confidence: "high",
            fingerprint: "sha256:0123456789abcdef".to_string(),
            test: identity(),
            message: "The assertion does not observe the Act result.".to_string(),
            primary: location("tests/api.rs", 24, 5, 35),
            evidence: vec![Evidence {
                kind: "act".to_string(),
                message: "The Act result is bound here.".to_string(),
                location: location("tests/api.rs", 20, 9, 38),
            }],
            why_it_matters: "The test can pass when behavior is broken.".to_string(),
            remediation: Remediation {
                summary: "Assert the response status.".to_string(),
                actions: vec!["Use an observable derived from `response`.".to_string()],
                example: Some("assert_eq!(response.status(), 201);".to_string()),
                rerun: "cntryl-tools validate-tests -f 'tests/api.rs'".to_string(),
            },
        });
        report
    }

    fn empty_report() -> ValidationReport {
        ValidationReport {
            schema_version: REPORT_SCHEMA_VERSION,
            tool: ToolInfo {
                name: "cntryl-tools",
                version: "0.1.0",
            },
            status: ReportStatus::Complete,
            root: ".".to_string(),
            summary: ValidationSummary {
                total_files: 0,
                total_tests: 0,
                fully_analyzed_tests: 0,
                partially_analyzed_tests: 0,
                tests_with_findings: 0,
                findings: 0,
                applied_exemptions: 0,
            },
            findings: Vec::new(),
            analysis_gaps: Vec::new(),
            exemptions: Vec::new(),
            errors: Vec::new(),
        }
    }

    fn identity() -> TestIdentity {
        TestIdentity {
            name: "should_return_created".to_string(),
            qualified_name: "api::should_return_created".to_string(),
        }
    }

    fn location(path: &str, line: usize, start_column: usize, end_column: usize) -> SourceLocation {
        SourceLocation {
            path: path.to_string(),
            span: SourceSpan {
                start: Position::new(line, start_column),
                end: Position::new(line, end_column),
            },
            label: "test location".to_string(),
        }
    }
}
