use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{ArgAction, Args};
use walkdir::WalkDir;

mod config;
mod model;
mod report;
mod rules;
mod rust;

use model::{
    AnalysisGap, OperationalError, ReportStatus, ToolInfo, ValidationReport, ValidationSummary,
    REPORT_SCHEMA_VERSION,
};

#[derive(Debug, Args, Clone)]
pub struct ValidateTestsArgs {
    /// Repository root used for discovery, configuration, and report paths.
    #[arg(long, default_value = ".", value_name = "PATH")]
    pub root: PathBuf,

    /// Analyze one Rust source file. May be repeated.
    #[arg(long, short = 'f', value_name = "PATH", action = ArgAction::Append)]
    pub file: Vec<PathBuf>,

    /// Override the repository policy file.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Write the versioned native JSON report.
    #[arg(long, short = 'j', value_name = "PATH")]
    pub json: Option<PathBuf>,

    /// Write a SARIF 2.1.0 report.
    #[arg(long, value_name = "PATH")]
    pub sarif: Option<PathBuf>,

    /// Fail an incomplete analysis after this duration (for example 500ms, 30s, or 2m).
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    pub timeout: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReportFormat {
    Json,
    Sarif,
}

impl ReportFormat {
    const fn name(self) -> &'static str {
        match self {
            Self::Json => "JSON",
            Self::Sarif => "SARIF",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportTarget {
    format: ReportFormat,
    path: PathBuf,
}

#[derive(Debug, Clone, Default)]
struct ReportPaths {
    json: Option<PathBuf>,
    sarif: Option<PathBuf>,
}

impl ReportPaths {
    fn resolve(root: &Path, args: &ValidateTestsArgs) -> Self {
        Self {
            json: args
                .json
                .as_deref()
                .map(|path| resolve_report_path(root, path)),
            sarif: args
                .sarif
                .as_deref()
                .map(|path| resolve_report_path(root, path)),
        }
    }

    fn targets(&self) -> Vec<ReportTarget> {
        let mut targets = Vec::with_capacity(2);
        if let Some(path) = &self.json {
            targets.push(ReportTarget {
                format: ReportFormat::Json,
                path: path.clone(),
            });
        }
        if let Some(path) = &self.sarif {
            targets.push(ReportTarget {
                format: ReportFormat::Sarif,
                path: path.clone(),
            });
        }
        targets
    }

    fn collides(&self) -> bool {
        self.json
            .as_ref()
            .zip(self.sarif.as_ref())
            .is_some_and(|(json, sarif)| report_paths_collide(json, sarif))
    }
}

fn comparable_report_path(path: &Path) -> PathBuf {
    let (mut resolved, missing) = report_path_parts(path);
    for component in missing {
        resolved.push(component);
    }
    resolved
}

fn report_path_parts(path: &Path) -> (PathBuf, Vec<std::ffi::OsString>) {
    let path = lexical_normalize(path);
    let mut existing = path.as_path();
    let mut missing = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name() else {
            return (path, Vec::new());
        };
        missing.push(name.to_os_string());
        let Some(parent) = existing.parent() else {
            return (path, Vec::new());
        };
        existing = parent;
    }
    missing.reverse();
    (
        fs::canonicalize(existing).unwrap_or_else(|_| existing.to_path_buf()),
        missing,
    )
}

fn report_paths_collide(left: &Path, right: &Path) -> bool {
    if comparable_report_path(left) == comparable_report_path(right) {
        return true;
    }
    let (left_existing, left_missing) = report_path_parts(left);
    let (right_existing, right_missing) = report_path_parts(right);
    left_existing == right_existing
        && filesystem_is_case_insensitive(&left_existing)
        && left_missing.len() == right_missing.len()
        && left_missing
            .iter()
            .zip(right_missing.iter())
            .all(|(left, right)| match (left.to_str(), right.to_str()) {
                (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
                _ => left == right,
            })
}

fn filesystem_is_case_insensitive(path: &Path) -> bool {
    for existing in path.ancestors() {
        let Some(name) = existing.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some((index, character)) = name
            .char_indices()
            .find(|(_, character)| character.is_ascii_alphabetic())
        else {
            continue;
        };
        let mut alternate_name = name.to_string();
        let replacement = if character.is_ascii_lowercase() {
            character.to_ascii_uppercase()
        } else {
            character.to_ascii_lowercase()
        };
        alternate_name.replace_range(
            index..index + character.len_utf8(),
            &replacement.to_string(),
        );
        let Some(parent) = existing.parent() else {
            continue;
        };
        let alternate = parent.join(alternate_name);
        return fs::canonicalize(&alternate)
            .ok()
            .zip(fs::canonicalize(existing).ok())
            .is_some_and(|(alternate, original)| alternate == original);
    }
    false
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

pub fn run(args: ValidateTestsArgs) -> Result<i32> {
    let started = Instant::now();
    let unresolved_report_paths = ReportPaths::resolve(&args.root, &args);
    let root = match fs::canonicalize(&args.root) {
        Ok(root) => root,
        Err(error) => {
            return operational_failure(
                &unresolved_report_paths,
                "input.read-failed",
                format!(
                    "failed to resolve repository root {}: {error}",
                    args.root.display()
                ),
            );
        }
    };
    let report_paths = ReportPaths::resolve(&root, &args);
    if report_paths.collides() {
        return operational_failure(
            &ReportPaths::default(),
            "input.invalid",
            "--json and --sarif must use different output paths".into(),
        );
    }
    if !root.is_dir() {
        return operational_failure(
            &report_paths,
            "input.invalid",
            format!("repository root is not a directory: {}", root.display()),
        );
    }
    if deadline_expired(started, args.timeout) {
        return operational_failure(
            &report_paths,
            "analysis.timeout",
            timeout_error(args.timeout).message,
        );
    }

    let policy = match config::load(&root, args.config.as_deref()) {
        Ok(policy) => policy,
        Err(error) => {
            return operational_failure(&report_paths, "config.invalid", format!("{error:#}"));
        }
    };
    if deadline_expired(started, args.timeout) {
        return operational_failure(
            &report_paths,
            "analysis.timeout",
            timeout_error(args.timeout).message,
        );
    }
    let files = match collect_files(
        &root,
        &args.file,
        &policy.source_roots,
        started,
        args.timeout,
    ) {
        Ok(files) => files,
        Err(error) => {
            if deadline_expired(started, args.timeout) {
                return operational_failure(
                    &report_paths,
                    "analysis.timeout",
                    timeout_error(args.timeout).message,
                );
            }
            return operational_failure(&report_paths, "input.invalid", format!("{error:#}"));
        }
    };
    let mut tests = Vec::new();
    let mut processed_files = 0usize;
    let mut errors = Vec::new();

    for file in &files {
        if deadline_expired(started, args.timeout) {
            errors.push(timeout_error(args.timeout));
            break;
        }

        let relative_path = relative_path(&root, file)?;
        let source = match fs::read_to_string(file) {
            Ok(source) => source,
            Err(error) => {
                errors.push(OperationalError {
                    code: "input.read-failed".into(),
                    message: format!("failed to read {relative_path}: {error}"),
                    location: None,
                });
                break;
            }
        };
        match rust::analyze_source(&relative_path, &source, &policy) {
            Ok(mut discovered) => tests.append(&mut discovered),
            Err(error) => {
                errors.push(OperationalError {
                    code: "input.parse-failed".into(),
                    message: format!("failed to parse {relative_path}: {error:#}"),
                    location: None,
                });
                break;
            }
        }
        processed_files += 1;

        if deadline_expired(started, args.timeout) {
            errors.push(timeout_error(args.timeout));
            break;
        }
    }

    if tests.is_empty() && errors.is_empty() {
        errors.push(OperationalError {
            code: "input.no-tests".into(),
            message: "no Rust tests were discovered in the selected files".into(),
            location: None,
        });
    }

    let evaluation = rules::evaluate(&tests, &policy);
    if deadline_expired(started, args.timeout)
        && !errors.iter().any(|error| error.code == "analysis.timeout")
    {
        errors.push(timeout_error(args.timeout));
    }
    let mut analysis_gaps: Vec<AnalysisGap> = tests
        .iter()
        .flat_map(|test| test.gaps.iter().cloned())
        .collect();
    analysis_gaps.extend(evaluation.analysis_gaps.iter().cloned());
    analysis_gaps.sort_by(|left, right| {
        left.location
            .path
            .cmp(&right.location.path)
            .then_with(|| left.test.qualified_name.cmp(&right.test.qualified_name))
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| {
                left.location
                    .span
                    .start
                    .line
                    .cmp(&right.location.span.start.line)
            })
    });

    let tests_with_findings = evaluation
        .findings
        .iter()
        .map(|finding| {
            (
                finding.primary.path.clone(),
                finding.test.qualified_name.clone(),
            )
        })
        .collect::<BTreeSet<_>>()
        .len();
    let partially_analyzed_tests = analysis_gaps
        .iter()
        .map(|gap| (gap.location.path.as_str(), gap.test.qualified_name.as_str()))
        .collect::<BTreeSet<_>>()
        .len();
    let mut report = ValidationReport {
        schema_version: REPORT_SCHEMA_VERSION,
        tool: ToolInfo {
            name: "cntryl-tools",
            version: env!("CARGO_PKG_VERSION"),
        },
        status: if errors.is_empty() {
            ReportStatus::Complete
        } else {
            ReportStatus::Incomplete
        },
        root: ".".into(),
        summary: ValidationSummary {
            total_files: processed_files,
            total_tests: tests.len(),
            fully_analyzed_tests: tests.len().saturating_sub(partially_analyzed_tests),
            partially_analyzed_tests,
            tests_with_findings,
            findings: evaluation.findings.len(),
            applied_exemptions: evaluation.exemptions.len(),
        },
        findings: evaluation.findings,
        analysis_gaps,
        exemptions: evaluation.exemptions,
        errors,
    };
    if deadline_expired(started, args.timeout)
        && !report
            .errors
            .iter()
            .any(|error| error.code == "analysis.timeout")
    {
        report.status = ReportStatus::Incomplete;
        report.errors.push(timeout_error(args.timeout));
    }

    finish_report(&report_paths, report)
}

fn operational_failure(paths: &ReportPaths, code: &str, message: String) -> Result<i32> {
    let report = ValidationReport {
        schema_version: REPORT_SCHEMA_VERSION,
        tool: ToolInfo {
            name: "cntryl-tools",
            version: env!("CARGO_PKG_VERSION"),
        },
        status: ReportStatus::Incomplete,
        root: ".".into(),
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
        errors: vec![OperationalError {
            code: code.into(),
            message,
            location: None,
        }],
    };
    finish_report(paths, report)
}

fn finish_report(paths: &ReportPaths, mut report: ValidationReport) -> Result<i32> {
    let (successful_targets, failures) = write_reports(paths, &report);
    if !failures.is_empty() {
        report.status = ReportStatus::Incomplete;
        report
            .errors
            .extend(failures.into_iter().map(|failure| OperationalError {
                code: "report.write-failed".into(),
                message: failure,
                location: None,
            }));

        for target in successful_targets {
            if let Err(error) = write_report(&target, &report) {
                let cleanup = fs::remove_file(&target.path);
                let cleanup_message = cleanup.err().map_or_else(String::new, |cleanup_error| {
                    format!("; also failed to remove the stale report: {cleanup_error}")
                });
                report.errors.push(OperationalError {
                    code: "report.write-failed".into(),
                    message: format!(
                        "failed to replace {} report {} with an incomplete report: {error:#}{cleanup_message}",
                        target.format.name(),
                        target.path.display()
                    ),
                    location: None,
                });
            }
        }
    }

    report::print_human(&report)?;
    Ok(report::exit_code(&report))
}

fn write_reports(
    paths: &ReportPaths,
    report: &ValidationReport,
) -> (Vec<ReportTarget>, Vec<String>) {
    let mut successful = Vec::new();
    let mut failures = Vec::new();
    for target in paths.targets() {
        match write_report(&target, report) {
            Ok(()) => successful.push(target),
            Err(error) => failures.push(format!(
                "failed to emit {} report {}: {error:#}",
                target.format.name(),
                target.path.display()
            )),
        }
    }
    (successful, failures)
}

fn write_report(target: &ReportTarget, report: &ValidationReport) -> Result<()> {
    match target.format {
        ReportFormat::Json => report::write_json(&target.path, report),
        ReportFormat::Sarif => report::write_sarif(&target.path, report),
    }
}

fn resolve_report_path(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn collect_files(
    root: &Path,
    requested: &[PathBuf],
    source_roots: &[String],
    started: Instant,
    timeout: Option<Duration>,
) -> Result<Vec<PathBuf>> {
    let mut files = BTreeSet::new();
    if requested.is_empty() {
        for configured_root in source_roots {
            if deadline_expired(started, timeout) {
                bail!("test analysis deadline exceeded during source discovery");
            }
            let candidate = root.join(configured_root);
            if !candidate.exists() {
                continue;
            }
            let source_root = canonical_inside_root(root, &candidate)?;
            if source_root.is_file() {
                add_rust_file(&mut files, &source_root)?;
                continue;
            }

            let walker = WalkDir::new(&source_root)
                .into_iter()
                .filter_entry(|entry| {
                    entry.depth() == 0
                        || (entry.file_name() != "target"
                            && !entry.file_name().to_string_lossy().starts_with('.'))
                });
            for entry in walker {
                if deadline_expired(started, timeout) {
                    bail!("test analysis deadline exceeded during source discovery");
                }
                let entry = entry.with_context(|| {
                    format!("failed while walking source root {}", source_root.display())
                })?;
                if entry.file_type().is_file()
                    && entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "rs")
                {
                    files.insert(canonical_inside_root(root, entry.path())?);
                }
            }
        }
    } else {
        for requested_file in requested {
            if deadline_expired(started, timeout) {
                bail!("test analysis deadline exceeded while resolving selected files");
            }
            let candidate = if requested_file.is_absolute() {
                requested_file.clone()
            } else {
                root.join(requested_file)
            };
            let file = canonical_inside_root(root, &candidate)?;
            add_rust_file(&mut files, &file)?;
        }
    }

    if files.is_empty() {
        bail!("no Rust source files found under the selected inputs");
    }
    Ok(files.into_iter().collect())
}

fn add_rust_file(files: &mut BTreeSet<PathBuf>, file: &Path) -> Result<()> {
    if !file.is_file() || file.extension().is_none_or(|extension| extension != "rs") {
        bail!(
            "test-validation input must be a Rust source file: {}",
            file.display()
        );
    }
    files.insert(file.to_path_buf());
    Ok(())
}

fn canonical_inside_root(root: &Path, candidate: &Path) -> Result<PathBuf> {
    let canonical = fs::canonicalize(candidate).with_context(|| {
        format!(
            "failed to resolve test-validation input {}",
            candidate.display()
        )
    })?;
    if !canonical.starts_with(root) {
        bail!(
            "test-validation input {} is outside repository root {}",
            canonical.display(),
            root.display()
        );
    }
    Ok(canonical)
}

fn relative_path(root: &Path, file: &Path) -> Result<String> {
    Ok(file
        .strip_prefix(root)
        .with_context(|| {
            format!(
                "test-validation input {} is outside repository root {}",
                file.display(),
                root.display()
            )
        })?
        .to_string_lossy()
        .replace('\\', "/"))
}

fn deadline_expired(started: Instant, timeout: Option<Duration>) -> bool {
    timeout.is_some_and(|limit| started.elapsed() >= limit)
}

fn timeout_error(timeout: Option<Duration>) -> OperationalError {
    OperationalError {
        code: "analysis.timeout".into(),
        message: format!(
            "test analysis exceeded its explicit {:?} deadline",
            timeout.unwrap_or_default()
        ),
        location: None,
    }
}

fn parse_duration(value: &str) -> std::result::Result<Duration, String> {
    let (number, multiplier) = if let Some(number) = value.strip_suffix("ms") {
        (number, 1_u64)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1_000)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60_000)
    } else {
        (value, 1_000)
    };
    let amount = number
        .parse::<u64>()
        .map_err(|_| format!("invalid duration `{value}`; use an integer with ms, s, or m"))?;
    let milliseconds = amount
        .checked_mul(multiplier)
        .ok_or_else(|| format!("duration `{value}` is too large"))?;
    Ok(Duration::from_millis(milliseconds))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::parse_duration;

    #[test]
    fn should_parse_supported_timeout_units() {
        // Arrange
        let values = [
            ("250ms", Duration::from_millis(250)),
            ("3s", Duration::from_secs(3)),
            ("2m", Duration::from_secs(120)),
            ("9", Duration::from_secs(9)),
        ];
        let expected = values.map(|(_, expected)| Ok(expected));

        // Act
        let parsed = values.map(|(value, _)| parse_duration(value));

        // Assert
        assert_eq!(parsed, expected);
    }
}
