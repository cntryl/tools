use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

pub const REPORT_SCHEMA_VERSION: &str = "cntryl.test-validation.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingCategory {
    Structure,
    Semantics,
    Hygiene,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Section {
    Body,
    Arrange,
    Act,
    Assert,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Position {
    pub line: usize,
    pub column: usize,
}

impl Position {
    pub fn new(line: usize, column: usize) -> Self {
        Self { line, column }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SourceSpan {
    pub start: Position,
    pub end: Position,
}

impl SourceSpan {
    pub fn line(line: usize, start_column: usize, end_column: usize) -> Self {
        Self {
            start: Position::new(line, start_column),
            end: Position::new(line, end_column),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceLocation {
    pub path: String,
    pub span: SourceSpan,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Evidence {
    pub kind: String,
    pub message: String,
    pub location: SourceLocation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Remediation {
    pub summary: String,
    pub actions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<String>,
    pub rerun: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub rule_id: String,
    pub category: FindingCategory,
    pub severity: &'static str,
    pub confidence: &'static str,
    pub fingerprint: String,
    pub test: TestIdentity,
    pub message: String,
    pub primary: SourceLocation,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
    pub why_it_matters: String,
    pub remediation: Remediation,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct TestIdentity {
    pub name: String,
    pub qualified_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AnalysisGap {
    pub code: String,
    pub test: TestIdentity,
    pub message: String,
    pub location: SourceLocation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppliedExemption {
    pub rule_id: String,
    pub test: TestIdentity,
    pub reason: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidationSummary {
    pub total_files: usize,
    pub total_tests: usize,
    pub fully_analyzed_tests: usize,
    pub partially_analyzed_tests: usize,
    pub tests_with_findings: usize,
    pub findings: usize,
    pub applied_exemptions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidationReport {
    pub schema_version: &'static str,
    pub tool: ToolInfo,
    pub status: ReportStatus,
    pub root: String,
    pub summary: ValidationSummary,
    pub findings: Vec<Finding>,
    pub analysis_gaps: Vec<AnalysisGap>,
    pub exemptions: Vec<AppliedExemption>,
    pub errors: Vec<OperationalError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolInfo {
    pub name: &'static str,
    pub version: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Complete,
    Incomplete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OperationalError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<SourceLocation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerKind {
    Arrange,
    Act,
    Assert,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    pub kind: MarkerKind,
    pub span: SourceSpan,
    pub combined: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallFact {
    pub name: String,
    pub qualified_name: String,
    pub span: SourceSpan,
    pub section: Section,
    pub receiver_identifiers: BTreeSet<String>,
    pub argument_identifiers: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleKind {
    Outcome,
    Interaction,
    BroadError,
    Execution,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleFact {
    pub kind: OracleKind,
    pub span: SourceSpan,
    pub section: Section,
    pub text: String,
    pub actual_identifiers: BTreeSet<String>,
    pub expected_identifiers: BTreeSet<String>,
    pub actual_calls: BTreeSet<String>,
    pub expected_calls: BTreeSet<String>,
    pub produced_identifiers: BTreeSet<String>,
    pub actual_root_call: Option<String>,
    pub expected_root_call: Option<String>,
    pub self_derived_candidate: bool,
    pub conditional: bool,
    pub tautological: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HygieneKind {
    FixedSleep,
    UncontrolledClock,
    UncontrolledRandom,
    ProcessEnvironment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HygieneFact {
    pub kind: HygieneKind,
    pub call: String,
    pub span: SourceSpan,
    pub section: Section,
    pub in_oracle: bool,
}

#[derive(Debug, Clone)]
pub struct TestCase {
    pub path: String,
    pub name: String,
    pub qualified_name: String,
    pub name_span: SourceSpan,
    pub function_span: SourceSpan,
    pub line_count: usize,
    pub markers: Vec<Marker>,
    pub section_statements: BTreeMap<Section, usize>,
    pub ignored: Option<String>,
    pub ignore_span: Option<SourceSpan>,
    pub should_panic: bool,
    pub should_panic_expected: Option<String>,
    pub act_outputs: BTreeSet<String>,
    pub act_effects: BTreeSet<String>,
    pub act_root_calls: BTreeSet<String>,
    pub uncontrolled_clock_outputs: BTreeSet<String>,
    pub oracles: Vec<OracleFact>,
    pub calls: Vec<CallFact>,
    pub hygiene: Vec<HygieneFact>,
    pub gaps: Vec<AnalysisGap>,
}
