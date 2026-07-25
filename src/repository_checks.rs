//! Generic repository-quality checks used by Cntryl repositories.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::Args;
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use walkdir::WalkDir;

fn root(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .with_context(|| format!("failed to resolve repository root {}", path.display()))
}

fn read_config<T: for<'de> Deserialize<'de> + Default>(
    root: &Path,
    path: &Option<PathBuf>,
) -> Result<T> {
    let path = path
        .clone()
        .unwrap_or_else(|| PathBuf::from(".cntryl/repository.toml"));
    let path = root.join(path);
    if !path.is_file() {
        return Ok(T::default());
    }
    toml::from_str(
        &fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?,
    )
    .with_context(|| format!("failed to parse {}", path.display()))
}

#[derive(Debug, Deserialize, Default)]
struct RepositoryConfig {
    #[serde(default)]
    docs: DocsConfig,
    #[serde(default)]
    benchmarks: BenchmarkConfig,
    #[serde(default)]
    module_sizes: ModuleSizeConfig,
}

#[derive(Debug, Args, Clone)]
pub struct ValidateDocsArgs {
    #[arg(long, default_value = ".")]
    pub root: PathBuf,
    #[arg(long)]
    pub config: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Default)]
struct DocsConfig {
    #[serde(default)]
    required: Vec<String>,
    #[serde(default)]
    exclude_paths: Vec<String>,
    #[serde(default)]
    forbidden_paths: Vec<String>,
    #[serde(default)]
    forbidden_text: Vec<String>,
}

pub fn validate_docs(args: ValidateDocsArgs) -> Result<i32> {
    let root = root(&args.root)?;
    let config = read_config::<RepositoryConfig>(&root, &args.config)?.docs;
    let markdown: Vec<PathBuf> = WalkDir::new(&root)
        .into_iter()
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            if entry.depth() > 0
                && (name == "target"
                    || name.starts_with('.')
                    || is_excluded(&root, entry.path(), &config.exclude_paths))
            {
                return false;
            }
            true
        })
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_file() && entry.path().extension().is_some_and(|ext| ext == "md")
        })
        .map(|entry| entry.into_path())
        .collect();
    let names: BTreeSet<String> = markdown.iter().map(|path| relative(&root, path)).collect();
    let mut errors = Vec::new();
    for path in &config.required {
        if !names.contains(path) {
            errors.push(format!("missing required document: {path}"));
        }
    }
    for path in &config.forbidden_paths {
        if names.contains(path) {
            errors.push(format!("forbidden document is present: {path}"));
        }
    }
    let mut headings = BTreeMap::new();
    for path in &markdown {
        let text = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let name = relative(&root, path);
        headings.insert(name.clone(), heading_ids(&text));
        for marker in &config.forbidden_text {
            if text.contains(marker) {
                errors.push(format!("{name}: forbidden documentation text {marker:?}"));
            }
        }
        for (target, anchor) in markdown_links(&text) {
            if target.is_empty() || target.contains("://") || target.starts_with("mailto:") {
                continue;
            }
            let target_path = path.parent().unwrap_or(&root).join(&target);
            let target_path = target_path.canonicalize().unwrap_or(target_path);
            if !target_path.is_file()
                && !(target_path.is_dir() && target_path.join("README.md").is_file())
            {
                errors.push(format!("{name}: broken local link {target}"));
            }
            if let Some(anchor) = anchor {
                let target_name = relative(&root, &target_path);
                if !headings
                    .get(&target_name)
                    .is_some_and(|ids| ids.contains(&slug(&anchor)))
                {
                    errors.push(format!("{name}: broken anchor {target}#{anchor}"));
                }
            }
        }
    }
    if errors.is_empty() {
        println!("validated {} Markdown documents", markdown.len());
        return Ok(0);
    }
    for error in errors {
        eprintln!("error: {error}");
    }
    Ok(1)
}

fn is_excluded(root: &Path, path: &Path, excluded: &[String]) -> bool {
    let name = relative(root, path);
    excluded.iter().any(|prefix| {
        let prefix = prefix.trim_end_matches('/');
        name == prefix || name.starts_with(&format!("{prefix}/"))
    })
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn slug(value: &str) -> String {
    value
        .chars()
        .map(|ch| ch.to_ascii_lowercase())
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == ' ' || *ch == '-')
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
}

fn heading_ids(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|line| line.strip_prefix('#'))
        .filter(|line| line.starts_with('#') || line.starts_with(' '))
        .map(|line| slug(line.trim_matches('#').trim()))
        .collect()
}

fn markdown_links(text: &str) -> Vec<(String, Option<String>)> {
    let Ok(re) = Regex::new(r"\[[^\]]*\]\(([^)#\s]*)(?:#([^)]*))?\)") else {
        return Vec::new();
    };
    re.captures_iter(text)
        .map(|capture| {
            (
                capture[1].to_string(),
                capture.get(2).map(|m| m.as_str().to_string()),
            )
        })
        .collect()
}

#[derive(Debug, Args, Clone)]
pub struct ValidateBenchmarksArgs {
    #[arg(long, default_value = ".")]
    pub root: PathBuf,
    #[arg(long)]
    pub config: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct BenchmarkConfig {
    #[serde(default = "default_benchmark_docs")]
    documentation: Vec<String>,
    #[serde(default = "default_benchmark_workflow")]
    workflow: String,
    #[serde(default)]
    not_pr_gate_phrase: Option<String>,
}

impl Default for BenchmarkConfig {
    fn default() -> Self {
        Self {
            documentation: default_benchmark_docs(),
            workflow: default_benchmark_workflow(),
            not_pr_gate_phrase: None,
        }
    }
}
fn default_benchmark_docs() -> Vec<String> {
    vec![
        "docs/development/benchmarks.md".into(),
        "docs/development/performance-targets.md".into(),
        "docs/operations/performance-tuning.md".into(),
    ]
}
fn default_benchmark_workflow() -> String {
    ".github/workflows/bench.yml".into()
}

pub fn validate_benchmarks(args: ValidateBenchmarksArgs) -> Result<i32> {
    let root = root(&args.root)?;
    let config = read_config::<RepositoryConfig>(&root, &args.config)?.benchmarks;
    let metadata: Value = serde_json::from_slice(
        &Command::new("cargo")
            .args(["metadata", "--no-deps", "--format-version", "1"])
            .current_dir(&root)
            .output()
            .context("failed to run cargo metadata")?
            .stdout,
    )
    .context("invalid cargo metadata")?;
    let package = metadata["packages"]
        .as_array()
        .and_then(|packages| {
            packages.iter().find(|package| {
                package["manifest_path"]
                    .as_str()
                    .is_some_and(|path| Path::new(path).parent() == Some(root.as_path()))
            })
        })
        .context("repository package not found in cargo metadata")?;
    let registered: BTreeSet<String> = package["targets"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter(|target| {
            target["kind"]
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|kind| kind == "bench"))
        })
        .filter_map(|target| target["name"].as_str().map(str::to_owned))
        .collect();
    let bench_re = Regex::new(r#"cargo\s+bench\s+--bench\s+['\"`]?([A-Za-z0-9_.*-]+)"#)?;
    let mut errors = Vec::new();
    let mut docs = String::new();
    for path in &config.documentation {
        let text = fs::read_to_string(root.join(path))
            .with_context(|| format!("failed to read {path}"))?;
        for capture in bench_re.captures_iter(&text) {
            let target = &capture[1];
            if !registered.contains(target) {
                errors.push(format!("{path} advertises unregistered benchmark {target}"));
            }
        }
        if text.contains("--save-baseline") || text.contains("--baseline") {
            errors.push(format!(
                "{path} advertises obsolete Criterion baseline flags"
            ));
        }
        docs.push_str(&text);
    }
    let workflow = fs::read_to_string(root.join(&config.workflow))
        .with_context(|| format!("failed to read {}", config.workflow))?;
    for target in registered {
        if !bench_re.captures_iter(&workflow).any(|capture| {
            capture[1] == target
                || capture[1].contains('*') && target.starts_with(capture[1].trim_end_matches('*'))
        }) {
            errors.push(format!("benchmark workflow does not execute {target}"));
        }
    }
    if !workflow.contains("workflow_dispatch:") {
        errors.push("benchmark workflow has no manual trigger".into());
    }
    if workflow.contains("pull_request:") {
        errors.push("benchmark workflow must not be a pull-request gate".into());
    }
    if let Some(phrase) = config.not_pr_gate_phrase {
        if !docs.contains(&phrase) {
            errors.push(format!(
                "benchmark docs do not contain required phrase {phrase:?}"
            ));
        }
    }
    if errors.is_empty() {
        println!("benchmark contract matches Cargo metadata and workflow coverage");
        return Ok(0);
    }
    for error in errors {
        eprintln!("error: {error}");
    }
    Ok(1)
}

#[derive(Debug, Args, Clone)]
pub struct CheckModuleSizesArgs {
    #[arg(long, default_value = ".")]
    pub root: PathBuf,
    #[arg(long)]
    pub config: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct ModuleSizeConfig {
    #[serde(default = "default_warn")]
    warn_lines: usize,
    #[serde(default = "default_max")]
    max_lines: usize,
    #[serde(default)]
    allowlist: Vec<String>,
    #[serde(default)]
    legacy_allowlist: Vec<String>,
}
impl Default for ModuleSizeConfig {
    fn default() -> Self {
        Self {
            warn_lines: default_warn(),
            max_lines: default_max(),
            allowlist: Vec::new(),
            legacy_allowlist: Vec::new(),
        }
    }
}
fn default_warn() -> usize {
    1_200
}
fn default_max() -> usize {
    1_600
}

pub fn check_module_sizes(args: CheckModuleSizesArgs) -> Result<i32> {
    let root = root(&args.root)?;
    let config = read_config::<RepositoryConfig>(&root, &args.config)?.module_sizes;
    let src = root.join("src");
    let mut failures = Vec::new();
    for entry in WalkDir::new(&src)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_type().is_file()
                && e.path().extension().is_some_and(|ext| ext == "rs")
                && e.file_name() != "tests.rs"
        })
    {
        let path = entry.path();
        let count = production_lines(&fs::read_to_string(path)?);
        let name = relative(&root, path);
        if count > config.warn_lines {
            println!(
                "{count:5} {name}{}",
                if config.allowlist.iter().any(|allowed| allowed == &name) {
                    " [allowlist]"
                } else {
                    ""
                }
            );
        }
        if count > config.max_lines
            && !config.allowlist.iter().any(|allowed| allowed == &name)
            && !config
                .legacy_allowlist
                .iter()
                .any(|allowed| allowed == &name)
        {
            failures.push(format!("{count:5} {name}"));
        }
    }
    if failures.is_empty() {
        return Ok(0);
    }
    eprintln!(
        "modules exceed configured maximum of {} lines:",
        config.max_lines
    );
    for failure in failures {
        eprintln!("  {failure}");
    }
    Ok(1)
}

fn production_lines(text: &str) -> usize {
    let mut count = 0;
    let mut skip = false;
    let mut depth = 0i32;
    for line in text.lines() {
        let trimmed = line.trim();
        if !skip && trimmed.contains("#[cfg(test)]") {
            skip = true;
            continue;
        }
        if skip {
            depth += line.matches('{').count() as i32;
            depth -= line.matches('}').count() as i32;
            if depth <= 0 && (line.contains('{') || line.contains(';')) {
                skip = false;
            }
        } else if !trimmed.is_empty() && !trimmed.starts_with("//") {
            count += 1;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::is_excluded;
    use std::path::Path;

    #[test]
    fn should_exclude_configured_directory_and_children() {
        let root = Path::new("/repo");
        assert!(is_excluded(
            root,
            Path::new("/repo/ui/node_modules/pkg/readme.md"),
            &["ui/node_modules".into()]
        ));
        assert!(!is_excluded(
            root,
            Path::new("/repo/ui/docs/readme.md"),
            &["ui/node_modules".into()]
        ));
    }
}

#[derive(Debug, Args, Clone)]
pub struct TestWatchdogArgs {
    #[arg(long)]
    pub suite: Option<String>,
    #[arg(long)]
    pub all: bool,
    #[arg(long)]
    pub pattern: Option<String>,
    #[arg(long, default_value_t = 60)]
    pub timeout: u64,
    #[arg(long)]
    pub continue_on_fail: bool,
    #[arg(long, default_value = ".")]
    pub root: PathBuf,
}

pub fn test_watchdog(args: TestWatchdogArgs) -> Result<i32> {
    if args.suite.is_none() && !args.all {
        bail!("provide --suite <name> or --all");
    }
    let root = root(&args.root)?;
    let suites = if let Some(suite) = args.suite {
        vec![suite]
    } else {
        fs::read_dir(root.join("tests"))?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                entry.path().file_stem().and_then(|stem| {
                    entry
                        .path()
                        .extension()
                        .is_some_and(|ext| ext == "rs")
                        .then(|| stem.to_string_lossy().into_owned())
                })
            })
            .collect()
    };
    let mut failed = false;
    for suite in suites {
        let output = Command::new("cargo")
            .args(["test", "--test", &suite, "--", "--list"])
            .current_dir(&root)
            .output()
            .with_context(|| format!("failed to list tests in {suite}"))?;
        if !output.status.success() {
            eprint!("{}", String::from_utf8_lossy(&output.stdout));
            return Ok(2);
        }
        for line in String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.strip_suffix(": test"))
        {
            if args
                .pattern
                .as_ref()
                .is_some_and(|pattern| !format!("{suite}::{line}").contains(pattern))
            {
                continue;
            }
            println!("=== RUN {suite}::{line} (timeout {}s) ===", args.timeout);
            let status = run_test(&root, &suite, line, args.timeout)?;
            if status != 0 {
                failed = true;
                if !args.continue_on_fail {
                    return Ok(if status == 124 { 124 } else { 2 });
                }
            }
        }
    }
    Ok(if failed { 2 } else { 0 })
}

fn run_test(root: &Path, suite: &str, name: &str, timeout: u64) -> Result<i32> {
    let mut child = Command::new("cargo")
        .args([
            "test",
            "--test",
            suite,
            name,
            "--",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .current_dir(root)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.code().unwrap_or(1));
        }
        if start.elapsed() >= Duration::from_secs(timeout) {
            child.kill()?;
            let _ = child.wait();
            eprintln!("=== TIMEOUT {suite}::{name} ===");
            return Ok(124);
        }
        thread::sleep(Duration::from_millis(100));
    }
}
