use datadiff::data;
use datadiff::schema;

use clap::{Parser, Subcommand};
use colored::Colorize;

#[derive(Parser)]
#[command(name = "datadiff")]
#[command(about = "A CLI tool for diffing data and schemas")]
#[command(arg_required_else_help = true)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    Schema {
        #[arg(short, long)]
        source: String,

        #[arg(short, long)]
        target: String,

        #[arg(long, help = "Optional path to a JSON schema policy/contract file")]
        policy: Option<String>,
    },
    Data {
        #[arg(short, long)]
        source: String,

        #[arg(short, long)]
        target: String,

        #[arg(short, long, required = true)]
        key: Vec<String>,

        #[arg(long)]
        output: Option<String>,

        #[arg(long, value_enum)]
        format: Option<data::ExportFormat>,

        #[arg(long)]
        temp: bool,

        #[arg(long, help = "Columns to exclude from comparison (comma-separated)")]
        exclude_columns: Option<String>,

        #[arg(long, help = "Only compare these columns (comma-separated)")]
        only_columns: Option<String>,

        #[arg(long, help = "Tolerance for numeric comparisons (0-1 for percentage, or absolute difference)")]
        numeric_tolerance: Option<f64>,

        #[arg(long, help = "Show only modified rows, skip summary tables")]
        diffs_only: bool,

        #[arg(long, help = "Output results as JSON to stdout (suppresses all other output)")]
        json: bool,
    },
    Batch {
        #[arg(long, help = "Path to a batch manifest describing source/target pairs (JSON or CSV)")]
        manifest: String,

        #[arg(long, value_enum, help = "Override manifest parsing format (json or csv)")]
        manifest_format: Option<data::ManifestFormat>,

        #[arg(short, long, required = true)]
        key: Vec<String>,

        #[arg(long)]
        output: Option<String>,

        #[arg(long, value_enum)]
        format: Option<data::ExportFormat>,

        #[arg(long, help = "Columns to exclude from comparison (comma-separated)")]
        exclude_columns: Option<String>,

        #[arg(long, help = "Only compare these columns (comma-separated)")]
        only_columns: Option<String>,

        #[arg(long, help = "Tolerance for numeric comparisons (0-1 for percentage, or absolute difference)")]
        numeric_tolerance: Option<f64>,

        #[arg(long, help = "Show only per-pair diff counts, skip verbose summaries")]
        diffs_only: bool,

        #[arg(long, help = "Stop the batch as soon as one pair fails")]
        fail_fast: bool,
    },
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{} {}", "error:".red().bold(), e);
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Schema {
            source,
            target,
            policy,
        } => {
            schema::schema_diff(&source, &target, policy.as_deref())?;
        }
        Commands::Data {
            source,
            target,
            key,
            output,
            format,
            temp,
            exclude_columns,
            only_columns,
            numeric_tolerance,
            diffs_only,
            json,
        } => {
            data::validate_export_args(output.as_deref(), format.as_ref(), temp)?;
            data::data_diff(
                &source,
                &target,
                &key,
                output.as_deref(),
                format,
                temp,
                exclude_columns.as_deref(),
                only_columns.as_deref(),
                numeric_tolerance,
                diffs_only,
                json,
            )?;
        }
        Commands::Batch {
            manifest,
            manifest_format,
            key,
            output,
            format,
            exclude_columns,
            only_columns,
            numeric_tolerance,
            diffs_only,
            fail_fast,
        } => {
            data::validate_export_args(output.as_deref(), format.as_ref(), false)?;
            data::batch_diff(
                &manifest,
                manifest_format,
                &key,
                output.as_deref(),
                format,
                exclude_columns.as_deref(),
                only_columns.as_deref(),
                numeric_tolerance,
                diffs_only,
                fail_fast,
            )?;
        }
    }

    Ok(())
}
