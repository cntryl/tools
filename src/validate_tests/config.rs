use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TestExemption {
    pub rule_id: String,
    pub path: String,
    pub test: String,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TestPolicyConfig {
    #[serde(default = "default_source_roots")]
    pub source_roots: Vec<String>,
    #[serde(default = "default_aaa_min_lines")]
    pub aaa_min_lines: usize,
    #[serde(default = "default_test_attributes")]
    pub test_attributes: Vec<String>,
    #[serde(default = "default_assertion_macros")]
    pub assertion_macros: Vec<String>,
    #[serde(default = "default_assertion_functions")]
    pub assertion_functions: Vec<String>,
    #[serde(default = "default_mock_interactions")]
    pub mock_interaction_methods: Vec<String>,
    #[serde(default)]
    pub controlled_time_functions: Vec<String>,
    #[serde(default)]
    pub controlled_random_functions: Vec<String>,
    #[serde(default)]
    pub controlled_environment_functions: Vec<String>,
    #[serde(default)]
    pub exemptions: Vec<TestExemption>,
}

impl Default for TestPolicyConfig {
    fn default() -> Self {
        Self {
            source_roots: default_source_roots(),
            aaa_min_lines: default_aaa_min_lines(),
            test_attributes: default_test_attributes(),
            assertion_macros: default_assertion_macros(),
            assertion_functions: default_assertion_functions(),
            mock_interaction_methods: default_mock_interactions(),
            controlled_time_functions: Vec::new(),
            controlled_random_functions: Vec::new(),
            controlled_environment_functions: Vec::new(),
            exemptions: Vec::new(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct RepositoryConfig {
    #[serde(default)]
    tests: TestPolicyConfig,
}

pub fn load(root: &Path, configured_path: Option<&Path>) -> Result<TestPolicyConfig> {
    let path = configured_path.map_or_else(
        || root.join(".cntryl/repository.toml"),
        |path| {
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                root.join(path)
            }
        },
    );
    if !path.exists() {
        if configured_path.is_some() {
            bail!("configured test policy does not exist: {}", path.display());
        }
        return Ok(TestPolicyConfig::default());
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read test policy {}", path.display()))?;
    let repository: RepositoryConfig = toml::from_str(&content)
        .with_context(|| format!("failed to parse test policy {}", path.display()))?;
    validate(&repository.tests, &path)?;
    Ok(repository.tests)
}

fn validate(config: &TestPolicyConfig, path: &Path) -> Result<()> {
    if config.source_roots.is_empty() {
        bail!("{}: tests.source_roots must not be empty", path.display());
    }
    for source_root in &config.source_roots {
        if !is_safe_relative_path(source_root) {
            bail!(
                "{}: tests.source_roots entries must be nonempty repository-relative paths",
                path.display()
            );
        }
    }
    if config.aaa_min_lines == 0 {
        bail!("{}: tests.aaa_min_lines must be positive", path.display());
    }
    let mut exemption_keys = BTreeSet::new();
    for exemption in &config.exemptions {
        if exemption.rule_id.trim().is_empty()
            || exemption.path.trim().is_empty()
            || exemption.test.trim().is_empty()
            || exemption.reason.trim().is_empty()
        {
            bail!(
                "{}: every tests.exemptions entry requires rule_id, path, test, and reason",
                path.display()
            );
        }
        if [&exemption.rule_id, &exemption.path, &exemption.test]
            .iter()
            .any(|value| value.contains('*') || value.contains('?'))
        {
            bail!(
                "{}: tests.exemptions must identify an exact rule, path, and test",
                path.display()
            );
        }
        if !is_safe_relative_path(&exemption.path) {
            bail!(
                "{}: tests.exemptions paths must be repository-relative and cannot traverse parents",
                path.display()
            );
        }
        let key = (
            exemption.rule_id.as_str(),
            normalize_path(&exemption.path),
            exemption.test.as_str(),
        );
        if !exemption_keys.insert(key) {
            bail!(
                "{}: duplicate tests.exemptions entry for {} {} {}",
                path.display(),
                exemption.rule_id,
                exemption.path,
                exemption.test
            );
        }
    }
    Ok(())
}

impl TestPolicyConfig {
    pub fn matches_test_attribute(&self, qualified: &str) -> bool {
        matches_configured_name(&self.test_attributes, qualified)
    }

    pub fn matches_assertion_macro(&self, qualified: &str) -> bool {
        matches_configured_name(&self.assertion_macros, qualified)
    }

    pub fn matches_assertion_function(&self, qualified: &str) -> bool {
        matches_configured_name(&self.assertion_functions, qualified)
    }

    pub fn matches_mock_interaction(&self, qualified: &str) -> bool {
        matches_configured_name(&self.mock_interaction_methods, qualified)
    }

    pub fn is_controlled_time(&self, qualified: &str) -> bool {
        matches_configured_name(&self.controlled_time_functions, qualified)
    }

    pub fn is_controlled_random(&self, qualified: &str) -> bool {
        matches_configured_name(&self.controlled_random_functions, qualified)
    }

    pub fn is_controlled_environment(&self, qualified: &str) -> bool {
        matches_configured_name(&self.controlled_environment_functions, qualified)
    }

    pub fn exemption(&self, rule_id: &str, path: &str, test: &str) -> Option<&TestExemption> {
        self.exemptions.iter().find(|exemption| {
            exemption.rule_id == rule_id
                && normalize_path(&exemption.path) == normalize_path(path)
                && exemption.test == test
        })
    }
}

fn matches_configured_name(configured: &[String], qualified: &str) -> bool {
    let final_segment = qualified.rsplit("::").next().unwrap_or(qualified);
    configured
        .iter()
        .any(|name| name == qualified || (!name.contains("::") && name == final_segment))
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_string()
}

fn is_safe_relative_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    !normalized.trim().is_empty()
        && !normalized.starts_with('/')
        && !normalized.split('/').any(|component| component == "..")
}

fn default_source_roots() -> Vec<String> {
    vec!["src".into(), "tests".into()]
}

const fn default_aaa_min_lines() -> usize {
    5
}

fn default_test_attributes() -> Vec<String> {
    [
        "test",
        "tokio::test",
        "async_std::test",
        "test_log::test",
        "rstest",
        "test_case",
        "quickcheck",
        "wasm_bindgen_test",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn default_assertion_macros() -> Vec<String> {
    [
        "assert",
        "assert_eq",
        "assert_ne",
        "debug_assert",
        "debug_assert_eq",
        "debug_assert_ne",
        "assert_matches",
        "prop_assert",
        "prop_assert_eq",
        "prop_assert_ne",
        "assert_snapshot",
        "assert_debug_snapshot",
        "assert_json_snapshot",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn default_assertion_functions() -> Vec<String> {
    vec!["assert_matches".into()]
}

fn default_mock_interactions() -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{load, TestPolicyConfig};

    #[test]
    fn should_load_tests_policy_with_unrelated_repository_sections() {
        // Arrange
        let repository = tempfile::tempdir().expect("create policy fixture");
        let policy_dir = repository.path().join(".cntryl");
        fs::create_dir_all(&policy_dir).expect("create policy directory");
        fs::write(
            policy_dir.join("repository.toml"),
            r#"
[docs]
required = ["README.md"]

[tests]
aaa_min_lines = 8
assertion_macros = ["assert", "expect_that"]
"#,
        )
        .expect("write test policy");

        // Act
        let config = load(repository.path(), None).expect("load test policy");

        // Assert
        assert_eq!(config.aaa_min_lines, 8);
        assert!(config.matches_assertion_macro("crate::expect_that"));
    }

    #[test]
    fn should_reject_broad_exemption() {
        // Arrange
        let repository = tempfile::tempdir().expect("create policy fixture");
        let policy = repository.path().join("policy.toml");
        fs::write(
            &policy,
            r#"
[[tests.exemptions]]
rule_id = "HYG*"
path = "src/lib.rs"
test = "should_use_clock"
reason = "clock behavior"
"#,
        )
        .expect("write test policy");

        // Act
        let error = load(repository.path(), Some(&policy)).expect_err("reject broad exemption");

        // Assert
        assert!(error.to_string().contains("exact rule"));
    }

    #[test]
    fn should_use_default_policy_when_config_is_absent() {
        // Arrange
        let repository = tempfile::tempdir().expect("create policy fixture");

        // Act
        let config = load(repository.path(), None).expect("load default policy");

        // Assert
        assert_eq!(config, TestPolicyConfig::default());
    }

    #[test]
    fn should_reject_missing_explicit_policy() {
        // Arrange
        let repository = tempfile::tempdir().expect("create policy fixture");

        // Act
        let error = load(repository.path(), Some(Path::new("missing.toml")))
            .expect_err("reject missing explicit policy");

        // Assert
        assert!(error.to_string().contains("does not exist"));
    }
}
