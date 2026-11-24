use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn package(name: &str) -> String {
    format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n")
}

fn good(name: &str) -> String {
    format!("#[test]\nfn should_{name}() {{\n    let value = 1;\n    assert_eq!(value, 1);\n}}\n")
}

fn bad(name: &str) -> String {
    format!("#[test]\nfn {name}() {{\n    assert!(true);\n}}\n")
}

fn command(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cntryl-tools"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run cntryl-tools")
}

fn with_report(cwd: &Path) -> (Output, Vec<Value>) {
    let output = command(cwd, &["validate-tests", "--json", "report.json"]);
    let json = fs::read(cwd.join("report.json")).unwrap_or_else(|error| {
        panic!(
            "missing report in {}: {error}\n{}",
            cwd.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    });
    let values = serde_json::from_slice(&json).unwrap();
    (output, values)
}

fn files(results: &[Value]) -> Vec<String> {
    results
        .iter()
        .map(|result| result["file"].as_str().unwrap().to_owned())
        .collect()
}

fn display(path: &str) -> String {
    path.split('/').collect::<PathBuf>().display().to_string()
}

fn virtual_workspace() -> TempDir {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "Cargo.toml",
        "[workspace]\nresolver = \"2\"\nmembers = [\"members/pkg*\"]\nexclude = [\"members/pkg-excluded\"]\ndefault-members = [\"members/pkg\"]\n",
    );
    // A virtual root's trees are not package test sources.
    write(
        root.path(),
        "src/tests/ignored.rs",
        &bad("virtual_root_test"),
    );
    write(
        root.path(),
        "members/pkg/Cargo.toml",
        &format!(
            "{}\n[dependencies]\npathdep = {{ path = \"../pathdep\" }}\n\n[[test]]\nname = \"custom\"\npath = \"custom/custom.rs\"\n",
            package("pkg")
        ),
    );
    write(
        root.path(),
        "members/pkg/src/lib.rs",
        "#[cfg(test)]\nmod tests {\n    #[test]\n    fn should_run() { assert!(true); }\n}\n",
    );
    write(
        root.path(),
        "members/pkg/src/tests/helper.rs",
        &bad("helper_bad_name"),
    );
    write(
        root.path(),
        "members/pkg/custom/custom.rs",
        &good("custom_target"),
    );
    write(
        root.path(),
        "members/pkg/src/nested/Cargo.toml",
        &package("nested_nonmember"),
    );
    write(
        root.path(),
        "members/pkg/src/nested/tests/hidden.rs",
        &bad("nested_hidden_test"),
    );
    // The path dependency is an implicit workspace member and has a build target.
    write(
        root.path(),
        "members/pathdep/Cargo.toml",
        &package("pathdep"),
    );
    write(
        root.path(),
        "members/pathdep/src/lib.rs",
        "#[cfg(test)]\nmod tests {\n    #[test]\n    fn should_run() { assert!(true); }\n}\n",
    );
    write(
        root.path(),
        "members/pathdep/tests/entry.rs",
        &good("pathdep_entry"),
    );
    write(
        root.path(),
        "members/pkg-sibling/Cargo.toml",
        &package("sibling"),
    );
    write(
        root.path(),
        "members/pkg-sibling/src/lib.rs",
        &bad("sibling_violation"),
    );
    write(
        root.path(),
        "members/pkg-excluded/Cargo.toml",
        &package("excluded"),
    );
    write(
        root.path(),
        "members/pkg-excluded/src/lib.rs",
        &bad("excluded_test"),
    );
    root
}

#[test]
fn should_report_the_same_workspace_findings_given_a_member_subdirectory() {
    // Arrange
    let workspace = virtual_workspace();
    let caller = workspace.path().join("members/pkg/src/tests");

    // Act
    let (root_output, root_results) = with_report(workspace.path());
    let (member_output, member_results) = with_report(&caller);

    // Assert
    assert_eq!(root_output.status.code(), Some(1));
    assert_eq!(member_output.status.code(), Some(1));
    assert_eq!(root_results, member_results);
    let paths = files(&root_results);
    let expected = [
        "members/pathdep/src/lib.rs",
        "members/pathdep/tests/entry.rs",
        "members/pkg/custom/custom.rs",
        "members/pkg/src/lib.rs",
        "members/pkg/src/tests/helper.rs",
        "members/pkg-sibling/src/lib.rs",
    ]
    .map(display);
    assert_eq!(paths, expected);
    assert_eq!(
        root_results
            .iter()
            .filter(|r| !r["issues"].as_array().unwrap().is_empty())
            .count(),
        2
    );
    assert!(caller.join("report.json").is_file());
    assert!(!workspace.path().join("Cargo.lock").exists());
}

#[test]
fn should_scan_test_helpers_given_a_flat_member_at_the_tests_directory() {
    // Arrange
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "Cargo.toml",
        &format!(
            "{}\n[workspace]\nmembers = [\"tests\"]\n",
            package("root_pkg")
        ),
    );
    write(root.path(), "src/lib.rs", &good("check_root_package"));
    write(
        root.path(),
        "tests/Cargo.toml",
        &format!(
            "{}\n[lib]\npath = \"support.rs\"\n\n[[test]]\nname = \"outside\"\npath = \"entry.rs\"\n",
            package("flat_tests")
        ),
    );
    write(root.path(), "tests/support.rs", "");
    write(
        root.path(),
        "tests/entry.rs",
        &format!(
            "#[path = \"helpers/helper.rs\"]\nmod helper;\n{}",
            good("explicit_entry")
        ),
    );
    write(
        root.path(),
        "tests/helpers/helper.rs",
        &bad("helper_has_bad_name"),
    );

    // Act
    let (output, results) = with_report(root.path());

    // Assert
    assert_eq!(output.status.code(), Some(1));
    let paths = files(&results);
    assert_eq!(
        paths,
        ["src/lib.rs", "tests/entry.rs", "tests/helpers/helper.rs",].map(display)
    );
    assert_eq!(results[2]["test"], "helper_has_bad_name");
    assert!(results[2]["issues"][0]
        .as_str()
        .unwrap()
        .starts_with("NAMING:"));
}

#[test]
fn should_scan_source_trees_given_a_standalone_package_or_no_manifest() {
    // Arrange
    let standalone = tempfile::tempdir().unwrap();
    write(standalone.path(), "Cargo.toml", &package("standalone"));
    write(standalone.path(), "src/lib.rs", &good("standalone_source"));
    write(
        standalone.path(),
        "tests/test.rs",
        &good("standalone_integration"),
    );
    let bare = tempfile::tempdir().unwrap();
    write(bare.path(), "src/lib.rs", &good("bare_source"));
    write(bare.path(), "tests/test.rs", &good("bare_integration"));
    write(
        bare.path(),
        "nested/src/lib.rs",
        &good("nested_bare_source"),
    );
    write(
        bare.path(),
        "nested/tests/test.rs",
        &good("nested_bare_integration"),
    );
    let nested = bare.path().join("nested");

    // Act
    let (standalone_output, standalone_results) = with_report(standalone.path());
    let (bare_output, bare_results) = with_report(bare.path());
    let (nested_output, nested_results) = with_report(&nested);

    // Assert
    assert_eq!(standalone_output.status.code(), Some(0));
    assert_eq!(bare_output.status.code(), Some(0));
    assert_eq!(nested_output.status.code(), Some(0));
    assert_eq!(standalone_results.len(), 2);
    assert_eq!(bare_results.len(), 2);
    assert_eq!(
        files(&nested_results),
        ["src/lib.rs", "tests/test.rs"].map(display)
    );
}

#[test]
fn should_keep_existing_json_given_a_discovery_or_source_read_failure() {
    // Arrange
    let malformed = tempfile::tempdir().unwrap();
    write(malformed.path(), "Cargo.toml", "[package\nname = \"bad\"\n");
    let unreadable = tempfile::tempdir().unwrap();
    write(unreadable.path(), "Cargo.toml", &package("broken_source"));
    write(unreadable.path(), "src/lib.rs", &good("valid_source"));
    fs::create_dir_all(unreadable.path().join("tests")).unwrap();
    fs::write(unreadable.path().join("tests/bad.rs"), [0xff, 0xfe]).unwrap();
    let cases = [
        (malformed.path(), "Cargo metadata failed"),
        (unreadable.path(), "failed to read"),
    ];
    for (root, _) in cases {
        write(root, "report.json", "preserve me\n");
    }

    // Act
    let outputs =
        cases.map(|(root, _)| command(root, &["validate-tests", "--json", "report.json"]));

    // Assert
    for ((root, diagnostic), output) in cases.into_iter().zip(outputs) {
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains(diagnostic));
        assert_eq!(
            fs::read_to_string(root.join("report.json")).unwrap(),
            "preserve me\n"
        );
    }
}

#[test]
fn should_validate_one_file_given_an_invalid_manifest_without_cargo() {
    // Arrange
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "Cargo.toml", "[package\nname = \"bad\"\n");
    write(root.path(), "valid.rs", &good("explicit_file"));
    let empty_path = root.path().join("empty-path");
    fs::create_dir(&empty_path).unwrap();

    // Act
    let explicit = Command::new(env!("CARGO_BIN_EXE_cntryl-tools"))
        .args(["validate-tests", "--file", "valid.rs"])
        .current_dir(root.path())
        .env("PATH", empty_path)
        .output()
        .unwrap();

    // Assert
    assert_eq!(explicit.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&explicit.stdout).contains("should_explicit_file"));
}
