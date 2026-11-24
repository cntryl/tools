# cntryl-tools

Standalone Rust command-line tool with child commands for cntryl.

## Tools

- `validate-tests`: checks Rust tests for naming and AAA conventions.
- `generate-inventory`: scans tests and benchmarks and writes an inventory report.
- `summarize-benchmarks`: collects benchmark results, compares them to baseline, and writes a report.
- `validate-docs`: validates configured Markdown inventory, links, anchors, and policy text.
- `validate-benchmarks`: validates Cargo benchmark targets against documentation and workflow coverage.

Benchmark repository configuration retains `benchmarks.workflow` as the primary
workflow. `benchmarks.additional_workflows` adds dedicated benchmark workflows to
the coverage union; every listed workflow must have a manual trigger and must
not run on pull requests. `[benchmarks.manual_targets]` maps explicitly manual
diagnostic target names to nonempty reasons. Each manual target must be registered
in Cargo and have an exact `cargo bench --bench TARGET` command in the configured
documentation. Unclassified targets still require workflow execution coverage.
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

### `validate-tests`

Run the command from the Cargo workspace root or from inside any member.

```bash
cntryl-tools validate-tests
cd crates/my-member
cntryl-tools validate-tests --json report.json
```

Both invocations scan every Cargo workspace member. The scan covers each
member's `src` and `tests` trees and the entry files of integration test
targets declared in Cargo. It walks through nested workspace members and
skips nested packages outside the active workspace. Use `--file` to
check a helper module outside the scanned trees.

Workspace scans require Cargo on `PATH`. Discovery reads Cargo metadata
without fetching dependencies. Without an ancestor `Cargo.toml`, the command
scans `src` and `tests` under the current directory. Single-file checks run
without Cargo.

Relative `--file` and `--json` arguments resolve from the invocation directory.
In a workspace scan, JSON finding paths are relative to the workspace root.
Files outside the workspace root appear with absolute paths.

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
```

Use `docs.exclude_paths` for generated or vendored trees that are not
repository-owned Markdown.

`summarize-benchmarks` also accepts `--product-name` and `--report-title` if you want to override the default report branding.

### Stress Reports

`summarize-benchmarks` reads cntryl-stress v0.3 artifacts from `target/stress/**/latest.json`.
Stress artifacts must use `schema_version: "cntryl-stress.v2"`.

The stress adapter normalizes current summaries into the shared benchmark manifest, including `throughput_ops_per_s`, `latency_p95_ns`, `ns_per_op`, `allocs_per_op`, and `bytes_per_op` records when present. It preserves structured parameters as report tags for sweep detection and writes `target/stress/stress_summary.csv` alongside `target/bench_results.json` and `target/bench_summary.md`. Non-current stress schemas are rejected so stale artifacts cannot silently enter reports.
