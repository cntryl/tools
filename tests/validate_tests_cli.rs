use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

const REPORT_SCHEMA_VERSION: &str = "cntryl.test-validation.v1";

fn repository_with_files(files: &[(&str, &str)]) -> TempDir {
    let repository = tempfile::tempdir().expect("create fixture repository");
    for (relative_path, source) in files {
        let path = repository.path().join(relative_path);
        fs::create_dir_all(path.parent().expect("fixture file parent"))
            .expect("create fixture directory");
        fs::write(&path, source).expect("write fixture file");
    }
    repository
}

fn run_validator(repository: &Path, arguments: &[&str]) -> Output {
    run_validator_from(repository, repository, arguments)
}

fn run_validator_from(repository: &Path, current_dir: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cntryl-tools"))
        .arg("validate-tests")
        .arg("--root")
        .arg(repository)
        .args(arguments)
        .current_dir(current_dir)
        .output()
        .expect("run validate-tests")
}

fn read_json(repository: &Path, relative_path: &str) -> Value {
    serde_json::from_slice(
        &fs::read(repository.join(relative_path)).expect("read JSON validation report"),
    )
    .expect("parse JSON validation report")
}

fn assert_exit_code(output: &Output, expected: i32) {
    assert_eq!(
        output.status.code(),
        Some(expected),
        "unexpected validate-tests exit status\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_sha256_fingerprint(value: &str) {
    let digest = value
        .strip_prefix("sha256:")
        .expect("fingerprint should have sha256 prefix");
    assert_eq!(
        digest.len(),
        64,
        "fingerprint should contain a SHA-256 digest"
    );
    assert!(
        digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "fingerprint should use lowercase hexadecimal"
    );
}

fn invalid_policy_outcome(policy: &str, index: usize) -> (Output, Value) {
    let repository = repository_with_files(&[
        (
            "src/lib.rs",
            r"
#[test]
fn should_report_observable_value() {
    let actual = parse_value();
    assert_eq!(actual, 42);
}
",
        ),
        (".cntryl/repository.toml", policy),
    ]);
    let output_name = format!("unknown-{index}.json");
    let output = run_validator(repository.path(), &["--json", output_name.as_str()]);
    let report = read_json(repository.path(), &output_name);
    (output, report)
}

#[test]
fn should_discover_attributed_async_test() {
    // Arrange
    let repository = repository_with_files(&[(
        "src/lib.rs",
        r"
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn should_observe_async_result() {
    let actual = service_call().await;
    assert_eq!(actual, 42);
}
",
    )]);
    // Act
    let output = run_validator(
        repository.path(),
        &["--file", "src/lib.rs", "--json", "async-report.json"],
    );
    assert_exit_code(&output, 0);
    let report = read_json(repository.path(), "async-report.json");

    // Assert
    assert_eq!(report["status"], "complete");
    assert_eq!(report["summary"]["total_files"], 1);
    assert_eq!(report["summary"]["total_tests"], 1);
    assert_eq!(report["summary"]["findings"], 0);
}

#[test]
fn should_exit_one_when_findings_are_reported() {
    // Arrange
    let repository = repository_with_files(&[(
        "src/lib.rs",
        r"
#[test]
fn reports_anything() {
    assert!(true);
}
",
    )]);

    // Act
    let output = run_validator(
        repository.path(),
        &["--file", "src/lib.rs", "--json", "findings.json"],
    );
    assert_exit_code(&output, 1);
    let report = read_json(repository.path(), "findings.json");

    // Assert
    assert_eq!(report["status"], "complete");
    assert_eq!(report["summary"]["tests_with_findings"], 1);
    assert!(report["summary"]["findings"].as_u64().unwrap_or(0) >= 1);
    assert!(report["findings"]
        .as_array()
        .is_some_and(|findings| !findings.is_empty()));
}

#[test]
fn should_exit_zero_when_analysis_is_clean() {
    // Arrange
    let repository = repository_with_files(&[(
        "src/lib.rs",
        r"
#[test]
fn should_report_observable_value() {
    let actual = parse_value();
    assert_eq!(actual, 42);
}
",
    )]);

    // Act
    let output = run_validator(
        repository.path(),
        &["--file", "src/lib.rs", "--json", "clean.json"],
    );
    assert_exit_code(&output, 0);
    let report = read_json(repository.path(), "clean.json");

    // Assert
    assert_eq!(report["status"], "complete");
    assert_eq!(report["summary"]["total_tests"], 1);
    assert_eq!(report["summary"]["findings"], 0);
    assert_eq!(report["findings"], Value::Array(Vec::new()));
    assert_eq!(report["errors"], Value::Array(Vec::new()));
}

#[test]
fn should_emit_deterministic_versioned_native_json() {
    // Arrange
    let repository = repository_with_files(&[(
        "src/lib.rs",
        r"
#[test]
fn reports_anything() {
    assert!(true);
}
",
    )]);

    // Act
    let first_output = run_validator(
        repository.path(),
        &["--file", "src/lib.rs", "--json", "first.json"],
    );
    let second_output = run_validator(
        repository.path(),
        &["--file", "src/lib.rs", "--json", "second.json"],
    );
    assert_exit_code(&first_output, 1);
    assert_exit_code(&second_output, 1);
    let first_bytes = fs::read(repository.path().join("first.json")).expect("read first report");
    let second_bytes = fs::read(repository.path().join("second.json")).expect("read second report");
    let report: Value = serde_json::from_slice(&first_bytes).expect("parse native JSON report");

    // Assert
    assert_eq!(
        first_bytes, second_bytes,
        "native JSON must be deterministic"
    );
    assert_eq!(report["schema_version"], REPORT_SCHEMA_VERSION);
    assert_eq!(report["tool"]["name"], "cntryl-tools");
    assert_eq!(report["status"], "complete");
    assert_eq!(
        report
            .as_object()
            .expect("native report object")
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "analysis_gaps",
            "errors",
            "exemptions",
            "findings",
            "root",
            "schema_version",
            "status",
            "summary",
            "tool",
        ])
    );
    let finding = &report["findings"][0];
    assert_sha256_fingerprint(
        finding["fingerprint"]
            .as_str()
            .expect("native finding fingerprint"),
    );
    assert!(finding["primary"]["span"]["start"]["line"]
        .as_u64()
        .is_some_and(|line| line > 0));
    assert!(finding["primary"]["span"]["start"]["column"]
        .as_u64()
        .is_some_and(|column| column > 0));
}

#[test]
fn should_emit_sarif_fingerprint_and_unicode_columns() {
    // Arrange
    let repository = repository_with_files(&[(
        "src/lib.rs",
        r"
#[test]
fn reports_anything() {
    assert!(true);
}
",
    )]);

    // Act
    let output = run_validator(
        repository.path(),
        &["--file", "src/lib.rs", "--sarif", "findings.sarif"],
    );
    assert_exit_code(&output, 1);
    let sarif = read_json(repository.path(), "findings.sarif");

    // Assert
    assert_eq!(sarif["version"], "2.1.0");
    assert!(sarif["$schema"]
        .as_str()
        .is_some_and(|schema| schema.ends_with("sarif-schema-2.1.0.json")));
    assert_eq!(sarif["runs"].as_array().map(Vec::len), Some(1));
    let run = &sarif["runs"][0];
    assert_eq!(run["columnKind"], "unicodeCodePoints");
    assert_eq!(run["invocations"][0]["exitCode"], 1);
    assert_eq!(run["invocations"][0]["executionSuccessful"], true);
    let result = &run["results"][0];
    assert_sha256_fingerprint(
        result["fingerprints"]["cntrylTestValidation/v1"]
            .as_str()
            .expect("SARIF result fingerprint"),
    );
    assert_eq!(
        result["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
        "src/lib.rs"
    );
    assert!(
        result["locations"][0]["physicalLocation"]["region"]["startColumn"]
            .as_u64()
            .is_some_and(|column| column > 0)
    );
}

#[test]
fn should_apply_config_to_repeated_file_filters() {
    // Arrange
    let repository = repository_with_files(&[
        (
            "src/first.rs",
            r"
#[test]
fn should_validate_first_file() {
    let input = 1;
    let actual = first(input);

    assert_eq!(actual, 1);
}
",
        ),
        (
            "tests/second.rs",
            r"
#[test]
fn should_validate_second_file() {
    let input = 2;
    let actual = second(input);

    assert_eq!(actual, 2);
}
",
        ),
        (
            "validation.toml",
            r"
[tests]
aaa_min_lines = 20
",
        ),
    ]);

    // Act
    let output = run_validator(
        repository.path(),
        &[
            "--file",
            "src/first.rs",
            "--file",
            "tests/second.rs",
            "--config",
            "validation.toml",
            "--json",
            "filtered.json",
        ],
    );
    assert_exit_code(&output, 0);
    let report = read_json(repository.path(), "filtered.json");

    // Assert
    assert_eq!(report["summary"]["total_files"], 2);
    assert_eq!(report["summary"]["total_tests"], 2);
    assert_eq!(report["summary"]["findings"], 0);
}

#[test]
fn should_exit_two_with_incomplete_report_when_timeout_expires() {
    // Arrange
    let repository = repository_with_files(&[(
        "src/lib.rs",
        r"
#[test]
fn should_report_observable_value() {
    let actual = parse_value();
    assert_eq!(actual, 42);
}
",
    )]);

    // Act
    let output = run_validator(
        repository.path(),
        &["--timeout", "0ms", "--json", "timeout.json"],
    );
    assert_exit_code(&output, 2);
    let report = read_json(repository.path(), "timeout.json");

    // Assert
    assert_eq!(report["status"], "incomplete");
    assert!(report["errors"]
        .as_array()
        .is_some_and(|errors| !errors.is_empty()));
    assert!(report["errors"].as_array().is_some_and(|errors| {
        errors.iter().any(|error| {
            error["code"]
                .as_str()
                .is_some_and(|code| code.contains("timeout"))
                || error["message"]
                    .as_str()
                    .is_some_and(|message| message.to_ascii_lowercase().contains("timeout"))
        })
    }));
}

#[test]
fn should_exit_two_and_report_invalid_configuration() {
    // Arrange
    let repository = repository_with_files(&[
        (
            "src/lib.rs",
            r"
#[test]
fn should_report_observable_value() {
    let actual = parse_value();
    assert_eq!(actual, 42);
}
",
        ),
        (
            ".cntryl/repository.toml",
            r"
[tests]
aaa_min_lines = 0
",
        ),
    ]);

    // Act
    let output = run_validator(
        repository.path(),
        &[
            "--json",
            "invalid-config.json",
            "--sarif",
            "invalid-config.sarif",
        ],
    );
    assert_exit_code(&output, 2);
    let report = read_json(repository.path(), "invalid-config.json");
    let sarif = read_json(repository.path(), "invalid-config.sarif");

    // Assert
    assert_eq!(report["status"], "incomplete");
    assert_eq!(report["errors"][0]["code"], "config.invalid");
    assert_eq!(report["summary"]["total_tests"], 0);
    assert_eq!(sarif["runs"][0]["results"], Value::Array(Vec::new()));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("error[config.invalid]"));
}

#[test]
fn should_reject_unknown_test_policy_keys() {
    // Arrange
    let policies = [
        r#"
[tests]
aaa_min_line = 5
"#,
        r#"
[[tests.exemptions]]
rule_id = "nondeterminism.fixed-sleep"
path = "src/lib.rs"
test = "should_wait_for_readiness"
reason = "external process"
expires = "2026-09-01"
"#,
    ];

    // Act
    let outcomes: Vec<_> = policies
        .into_iter()
        .enumerate()
        .map(|(index, policy)| invalid_policy_outcome(policy, index))
        .collect();

    // Assert
    assert!(outcomes
        .iter()
        .all(|(output, _)| output.status.code() == Some(2)));
    assert!(outcomes
        .iter()
        .all(|(_, report)| report["status"] == "incomplete"));
    assert!(outcomes
        .iter()
        .all(|(_, report)| report["errors"][0]["code"] == "config.invalid"));
    assert!(outcomes
        .iter()
        .all(|(_, report)| report["errors"][0]["message"]
            .as_str()
            .is_some_and(|message| message.contains("unknown field"))));
}

#[test]
fn should_resolve_relative_report_paths_against_root() {
    // Arrange
    let repository = repository_with_files(&[(
        "src/lib.rs",
        r"
#[test]
fn should_report_observable_value() {
    let actual = parse_value();
    assert_eq!(actual, 42);
}
",
    )]);
    let launcher = tempfile::tempdir().expect("create separate launcher directory");

    // Act
    let output = run_validator_from(
        repository.path(),
        launcher.path(),
        &[
            "--json",
            "reports/result.json",
            "--sarif",
            "reports/result.sarif",
        ],
    );
    assert_exit_code(&output, 0);
    let report = read_json(repository.path(), "reports/result.json");
    let sarif = read_json(repository.path(), "reports/result.sarif");

    // Assert
    assert_eq!(report["status"], "complete");
    assert_eq!(sarif["runs"][0]["results"], Value::Array(Vec::new()));
    assert!(!launcher.path().join("reports/result.json").exists());
    assert!(!launcher.path().join("reports/result.sarif").exists());
}

#[test]
fn should_reject_identical_resolved_report_paths_without_writing_a_report() {
    // Arrange
    let repository = repository_with_files(&[(
        "src/lib.rs",
        r"
#[test]
fn should_report_observable_value() {
    let actual = parse_value();
    assert_eq!(actual, 42);
}
",
    )]);
    let relative_path = "reports/shared-report.json";
    let absolute_path = repository.path().join(relative_path);
    let absolute_argument = absolute_path.to_string_lossy();

    // Act
    let output = run_validator(
        repository.path(),
        &[
            "--json",
            relative_path,
            "--sarif",
            absolute_argument.as_ref(),
        ],
    );

    // Assert
    assert_exit_code(&output, 2);
    assert!(!absolute_path.exists());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error["));
    assert!(stderr.contains("Validation incomplete"));
}

#[test]
fn should_reject_case_only_report_path_collision_on_case_insensitive_filesystem() {
    // Arrange
    let repository = repository_with_files(&[(
        "src/lib.rs",
        r"
#[test]
fn should_report_observable_value() {
    let actual = parse_value();
    assert_eq!(actual, 42);
}
",
    )]);
    let probe = repository.path().join("CaseProbe");
    fs::write(&probe, "probe").expect("write case-sensitivity probe");
    let case_insensitive = repository.path().join("caseprobe").exists();
    fs::remove_file(probe).expect("remove case-sensitivity probe");

    // Act
    let output = run_validator(
        repository.path(),
        &["--json", "Report.json", "--sarif", "report.json"],
    );

    // Assert
    assert_exit_code(&output, if case_insensitive { 2 } else { 0 });
    assert_eq!(
        repository.path().join("Report.json").exists(),
        !case_insensitive
    );
    assert_eq!(
        repository.path().join("report.json").exists(),
        !case_insensitive
    );
}

#[test]
fn should_rewrite_successful_sibling_report_as_incomplete_when_output_fails() {
    // Arrange
    let repository = repository_with_files(&[(
        "src/lib.rs",
        r"
#[test]
fn should_report_observable_value() {
    let actual = parse_value();
    assert_eq!(actual, 42);
}
",
    )]);
    fs::create_dir(repository.path().join("blocked.sarif"))
        .expect("create unwritable SARIF target");

    // Act
    let output = run_validator(
        repository.path(),
        &["--json", "successful.json", "--sarif", "blocked.sarif"],
    );
    assert_exit_code(&output, 2);
    let report = read_json(repository.path(), "successful.json");

    // Assert
    assert_eq!(report["status"], "incomplete");
    assert!(report["errors"].as_array().is_some_and(|errors| {
        errors
            .iter()
            .any(|error| error["code"] == "report.write-failed")
    }));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("error[report.write-failed]"));
}
