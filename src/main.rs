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
}

fn main() {
    let cli = Cli::parse();
    let exit_code = match cli.command {
        Commands::ValidateTests(args) => validate_tests::run(args),
        Commands::GenerateInventory(args) => generate_inventory::run(&args),
        Commands::SummarizeBenchmarks(args) => summarize_benchmarks::run(args),
    }
    .unwrap_or_else(|error| {
        eprintln!("error: {error:#}");
        1
    });

    std::process::exit(exit_code);
}
