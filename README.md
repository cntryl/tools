# cntryl-tools

Standalone Rust command-line tool with child commands for cntryl.

## Tools

- `validate-tests`: statically checks Rust test structure, oracle causality, and deterministic test hygiene.
- `generate-inventory`: scans tests and benchmarks and writes an inventory report.
- `summarize-benchmarks`: collects benchmark results, compares them to baseline, and writes a report.
- `validate-docs`: validates configured Markdown inventory, links, anchors, and policy text.
- `validate-benchmarks`: validates Cargo benchmark targets against documentation and workflow coverage.
- `check-module-sizes`: checks production Rust module sizes with configured thresholds and allowlists.
- `test-watchdog`: runs integration tests one at a time with per-test timeouts.

## Install

If you do not have the repo cloned, install directly from GitHub:

```bash
cargo install --git https://github.com/cntryl/tools --locked
```

If you are working from a local clone, install from that checkout instead:

```bash
cargo install --path /path/to/cntryl-tools
```

If you are already in the `cntryl-tools` repo, this shorter form does the same thing:

```bash
cargo install --path .
```

## Usage

Run them from the repository you want to inspect after install:

```bash
cntryl-tools validate-tests
cntryl-tools generate-inventory
cntryl-tools summarize-benchmarks
cntryl-tools validate-docs
cntryl-tools validate-benchmarks
cntryl-tools check-module-sizes
cntryl-tools test-watchdog --suite <suite> --timeout 60
```

`validate-tests` runs against the current directory by default, so run it from the repo you want to check.
It parses Rust with `syn`, recognizes common async and parameterized test attributes, and blocks only
high-confidence findings. Unsupported assertion helpers or macros are reported as nonblocking analysis
gaps instead of guessed violations.

### Test validation contract

1. **What is enforced:** behavior-oriented `should_` names; exact nonempty Arrange/Act/Assert sections
   for longer tests; one visible Act; explained ignores; specific panic expectations; an observable
   post-Act oracle; provably vacuous or disconnected assertions; and direct use of known fixed sleeps,
   uncontrolled clocks or randomness, and process-environment mutation.
2. **How it is enforced:** a Rust AST plus bounded, function-local dataflow connects Act outputs and
   explicit mutation carriers to post-Act observations. Known imports, assertion APIs, virtual-time
   controls, and repository configuration are resolved when the evidence is deterministic. Ambiguous
   receivers, interior mutation, opaque macros, overloaded computations, broad error checks,
   same-invocation expected values, and interaction-only mocks remain nonblocking analysis gaps.
3. **Feedback for remediation:** each blocking finding identifies the rule and test, points to evidence,
   explains the risk, proposes a smallest useful repair, and supplies a focused rerun command. Analysis
   gaps identify the exact syntax or intent that needs human review without failing an otherwise complete
   invocation.

Use repeatable `--file` filters for a focused scan, `--root` and `--config` for repository selection, and
an explicit `--timeout` such as `30s` when CI needs a deadline. The default has no deadline. Native JSON
and SARIF reports include stable rule IDs, fingerprints, evidence, why the issue matters, a remediation,
an example when useful, and a focused rerun command. Relative report paths are resolved from `--root`:

```bash
cntryl-tools validate-tests --file src/lib.rs --file tests/api.rs
cntryl-tools validate-tests --json target/test-validation.json
cntryl-tools validate-tests --sarif target/test-validation.sarif
```

Exit code `0` means a complete clean analysis, `1` means complete analysis with blocking findings, and
`2` means the analysis was incomplete because of invalid input or configuration, a parse/read failure,
a timeout, or a report-writing failure.

If you do not want to install the tool yet, you can run it with Cargo from any repo:

```bash
cargo run --manifest-path <path-to-cntryl-tools-repo>/Cargo.toml -- validate-tests
cargo run --manifest-path <path-to-cntryl-tools-repo>/Cargo.toml -- generate-inventory
cargo run --manifest-path <path-to-cntryl-tools-repo>/Cargo.toml -- summarize-benchmarks
```

If you are already in the repo you want to inspect, just run the installed commands there:

```bash
cntryl-tools validate-tests
cntryl-tools generate-inventory
cntryl-tools summarize-benchmarks
cntryl-tools validate-docs
cntryl-tools validate-benchmarks
cntryl-tools check-module-sizes
cntryl-tools test-watchdog --suite <suite> --timeout 60
```

## Repository policy

Checks use `.cntryl/repository.toml` when present. The file keeps project policy
out of the shared binary:

```toml
[docs]
required = ["README.md", "docs/README.md"]
exclude_paths = ["ui/node_modules"]
forbidden_paths = ["docs/archive/old.md"]
forbidden_text = ["placeholder text"]

[benchmarks]
documentation = ["docs/development/benchmarks.md"]
workflow = ".github/workflows/bench.yml"
not_pr_gate_phrase = "not a pull-request performance gate"

[module_sizes]
warn_lines = 1200
max_lines = 1600
allowlist = ["src/large_provider.rs"]
legacy_allowlist = []

[tests]
source_roots = ["src", "tests"]
aaa_min_lines = 5
assertion_functions = ["assert_domain_record"]
mock_interaction_methods = ["verify_expected_publish"]
controlled_time_functions = ["test_clock::now"]

[[tests.exemptions]]
rule_id = "nondeterminism.fixed-sleep"
path = "tests/external_process.rs"
test = "should_stop_external_process"
reason = "The external process exposes no readiness signal; tracked by issue #123."
```

Use `docs.exclude_paths` for generated or vendored trees that are not
repository-owned Markdown.

Test exemptions must identify one exact rule, repository-relative path, and qualified test name, and
must include a nonempty reason. Wildcards and unknown test-policy or exemption keys are rejected. Prefer
configuring known assertion helpers and controlled test utilities over exemptions so the analyzer can
retain causal evidence.

`summarize-benchmarks` also accepts `--product-name` and `--report-title` if you want to override the default report branding.

### Stress Reports

`summarize-benchmarks` reads cntryl-stress v0.3 artifacts from `target/stress/**/latest.json`.
Stress artifacts must use `schema_version: "cntryl-stress.v2"`.

The stress adapter normalizes current summaries into the shared benchmark manifest, including `throughput_ops_per_s`, `latency_p95_ns`, `ns_per_op`, `allocs_per_op`, and `bytes_per_op` records when present. It preserves structured parameters as report tags for sweep detection and writes `target/stress/stress_summary.csv` alongside `target/bench_results.json` and `target/bench_summary.md`. Non-current stress schemas are rejected so stale artifacts cannot silently enter reports.
