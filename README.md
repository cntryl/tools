# cntryl-tools

Standalone Rust command-line tool with child commands for cntryl.

## Tools

- `validate-tests`: checks Rust tests for naming and AAA conventions.
- `generate-inventory`: scans tests and benchmarks and writes an inventory report.
- `summarize-benchmarks`: collects benchmark results, compares them to baseline, and writes a report.

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
```

`validate-tests` runs against the current directory by default, so run it from the repo you want to check.

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
```

`summarize-benchmarks` also accepts `--product-name` and `--report-title` if you want to override the default report branding.

### Stress Reports

`summarize-benchmarks` reads cntryl-stress v0.3 artifacts from `target/stress/**/latest.json`.
Stress artifacts must use `schema_version: "cntryl-stress.v1"`.

The stress adapter normalizes current summaries into the shared benchmark manifest, including `throughput_ops_per_s`, `latency_p95_ns`, `ns_per_op`, `allocs_per_op`, and `bytes_per_op` records when present. It preserves structured parameters as report tags for sweep detection and writes `target/stress/stress_summary.csv` alongside `target/bench_results.json` and `target/bench_summary.md`. Non-current stress schemas are rejected so stale artifacts cannot silently enter reports.
