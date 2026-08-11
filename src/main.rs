#[path = "summarize_benchmarks/adapters/mod.rs"]
mod adapters;
#[path = "summarize_benchmarks/classify.rs"]
mod classify;
#[path = "summarize_benchmarks/compare.rs"]
mod compare;
#[path = "summarize_benchmarks/config.rs"]
mod config;
mod generate_inventory;
#[path = "summarize_benchmarks/model.rs"]
mod model;
#[path = "summarize_benchmarks/report.rs"]
mod report;
mod repository_checks;
mod summarize_benchmarks;
#[path = "summarize_benchmarks/sweep.rs"]
mod sweep;
mod validate_tests;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "cntryl-tools")]
#[command(about = "Standalone command-line tool with child commands for cntryl")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    #[command(name = "validate-tests")]
    ValidateTests(validate_tests::ValidateTestsArgs),
    #[command(name = "generate-inventory")]
    GenerateInventory(generate_inventory::GenerateInventoryArgs),
    #[command(name = "summarize-benchmarks")]
    SummarizeBenchmarks(summarize_benchmarks::SummarizeBenchmarksArgs),
    #[command(name = "validate-docs")]
    ValidateDocs(repository_checks::ValidateDocsArgs),
    #[command(name = "validate-benchmarks")]
    ValidateBenchmarks(repository_checks::ValidateBenchmarksArgs),
    #[command(name = "check-module-sizes")]
    CheckModuleSizes(repository_checks::CheckModuleSizesArgs),
    #[command(name = "test-watchdog")]
    TestWatchdog(repository_checks::TestWatchdogArgs),
}

fn main() {
    let cli = Cli::parse();
    let exit_code = match cli.command {
        Commands::ValidateTests(args) => validate_tests::run(args).unwrap_or_else(|error| {
            eprintln!("error: {error:#}");
            2
        }),
        Commands::GenerateInventory(args) => command_exit_code(generate_inventory::run(&args)),
        Commands::SummarizeBenchmarks(args) => command_exit_code(summarize_benchmarks::run(args)),
        Commands::ValidateDocs(args) => command_exit_code(repository_checks::validate_docs(args)),
        Commands::ValidateBenchmarks(args) => {
            command_exit_code(repository_checks::validate_benchmarks(args))
        }
        Commands::CheckModuleSizes(args) => {
            command_exit_code(repository_checks::check_module_sizes(args))
        }
        Commands::TestWatchdog(args) => command_exit_code(repository_checks::test_watchdog(args)),
    };

    std::process::exit(exit_code);
}

fn command_exit_code(result: anyhow::Result<i32>) -> i32 {
    result.unwrap_or_else(|error| {
        eprintln!("error: {error:#}");
        1
    })
}
