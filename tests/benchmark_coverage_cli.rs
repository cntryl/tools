use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "benchmark-coverage-{}-{timestamp}-{id}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixture(config: &str, extra_workflow: &str, documentation: &str) -> TempDir {
    let root = TempDir::new();
    fs::create_dir_all(root.path().join(".cntryl")).unwrap();
    fs::write(root.path().join(".cntryl/repository.toml"), config).unwrap();
    fs::write(root.path().join("Cargo.toml"), "[package]\nname='coverage-fixture'\nversion='0.1.0'\nedition='2021'\nautobenches=false\n[[bench]]\nname='tier1_hot'\npath='bench.rs'\n[[bench]]\nname='tier5_saturation'\npath='bench.rs'\n[[bench]]\nname='pressure'\npath='bench.rs'\n").unwrap();
    fs::write(root.path().join("bench.rs"), "fn main() {}\n").unwrap();
    fs::write(
        root.path().join("daily.yml"),
        "on:\n  workflow_dispatch:\nrun: cargo bench --bench 'tier1*'\n",
    )
    .unwrap();
    fs::write(root.path().join("stress.yml"), extra_workflow).unwrap();
    fs::write(root.path().join("benchmarks.md"), documentation).unwrap();
    root
}

const CONFIG: &str = "[benchmarks]\ndocumentation=['benchmarks.md']\nworkflow='daily.yml'\nadditional_workflows=['stress.yml']\n[benchmarks.manual_targets]\npressure='Bounded diagnostic requires explicit resource envelope'\n";
const STRESS: &str = "on:\n  workflow_dispatch:\nrun: cargo bench --locked --bench 'tier5_*'\n";
const DOCS: &str = "Manual diagnostic: cargo bench --bench pressure\n";

fn validate(root: &TempDir) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_cntryl-tools"))
        .args(["validate-benchmarks", "--root"])
        .arg(root.path())
        .output()
        .unwrap()
}

#[test]
fn should_cover_separate_workflows_and_documented_manual_diagnostics() {
    // Arrange
    let root = fixture(CONFIG, STRESS, DOCS);
    // Act
    let output = validate(&root);
    // Assert
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn should_reject_unregistered_manual_target() {
    // Arrange
    let root = fixture(&CONFIG.replace("pressure=", "typo="), STRESS, DOCS);
    // Act
    let output = validate(&root);
    // Assert
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("manual benchmark target typo is unregistered"));
}

#[test]
fn should_reject_manual_target_without_documented_command() {
    // Arrange
    let root = fixture(CONFIG, STRESS, "No diagnostic command here.");
    // Act
    let output = validate(&root);
    // Assert
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("has no documented command"));
}

#[test]
fn should_reject_pull_request_gate_in_additional_workflow() {
    // Arrange
    let root = fixture(CONFIG, &format!("{STRESS}  pull_request:\n"), DOCS);
    // Act
    let output = validate(&root);
    // Assert
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("must not be a pull-request gate"));
}

#[test]
fn should_reject_additional_workflow_without_manual_trigger() {
    // Arrange
    let root = fixture(CONFIG, &STRESS.replace("  workflow_dispatch:\n", ""), DOCS);
    // Act
    let output = validate(&root);
    // Assert
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("stress.yml has no manual trigger"));
}

#[test]
fn should_reject_empty_manual_target_reason() {
    // Arrange
    let root = fixture(
        &CONFIG.replace("Bounded diagnostic requires explicit resource envelope", ""),
        STRESS,
        DOCS,
    );
    // Act
    let output = validate(&root);
    // Assert
    assert!(String::from_utf8_lossy(&output.stderr).contains("has no reason"));
    assert!(!output.status.success());
}

#[test]
fn should_reject_uncovered_target_without_explicit_manual_classification() {
    // Arrange
    let root = fixture(
        CONFIG.split("[benchmarks.manual_targets]").next().unwrap(),
        STRESS,
        DOCS,
    );
    // Act
    let output = validate(&root);
    // Assert
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("benchmark workflow does not execute pressure"));
    assert!(!output.status.success());
}
