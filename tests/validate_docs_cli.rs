use std::fs;
use std::process::{Command, Output};

fn run_validator(link: &str) -> Output {
    let repository = tempfile::tempdir().expect("create fixture repository");
    fs::create_dir(repository.path().join("src")).expect("create source directory");
    fs::write(repository.path().join("src/lib.rs"), "one\ntwo\nthree\n")
        .expect("write source fixture");
    fs::write(repository.path().join("guide.md"), "# Guide\n\nDetails\n")
        .expect("write Markdown fixture");
    fs::write(
        repository.path().join("README.md"),
        format!("# Readme\n\n[Reference]({link})\n\n## Details\n"),
    )
    .expect("write referring document");
    Command::new(env!("CARGO_BIN_EXE_cntryl-tools"))
        .arg("validate-docs")
        .arg("--root")
        .arg(repository.path())
        .output()
        .expect("run documentation validator")
}

#[test]
fn should_accept_existing_source_and_markdown_line_anchors() {
    // Arrange
    let links = ["src/lib.rs#L1", "src/lib.rs#L2-L3", "guide.md#L3"];

    // Act
    let outputs = links.map(run_validator);

    // Assert
    for output in outputs {
        assert!(
            output.status.success(),
            "valid line reference failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn should_reject_source_line_anchors_outside_the_existing_file() {
    // Arrange
    let links = [
        "src/lib.rs#L0",
        "src/lib.rs#L4",
        "src/lib.rs#L1-L4",
        "src/lib.rs#L3-L2",
        "src/lib.rs#L1-Lx",
        "src/lib.rs#L999999999999999999999999999",
        "src/missing.rs#L1",
    ];

    // Act
    let outputs = links.map(run_validator);

    // Assert
    for output in outputs {
        assert_eq!(output.status.code(), Some(1));
    }
}

#[test]
fn should_preserve_markdown_heading_validation_when_line_anchors_are_supported() {
    // Arrange
    let valid_heading = "README.md#details";
    let missing_heading = "README.md#missing";

    // Act
    let valid = run_validator(valid_heading);
    let missing = run_validator(missing_heading);

    // Assert
    assert!(valid.status.success());
    assert_eq!(missing.status.code(), Some(1));
}
