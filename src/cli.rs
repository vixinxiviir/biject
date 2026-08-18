//! Command-line surface for the free commands.
//!
//! `Commands` and [`dispatch`] are public so that a downstream binary can embed
//! the free command set (via `#[command(flatten)]`) alongside commands of its
//! own, without duplicating the argument definitions or the dispatch logic.

use clap::{Parser, Subcommand};

use crate::data;
use crate::schema;

#[derive(Parser)]
#[command(name = "biject")]
#[command(about = "A CLI tool for diffing data and schemas")]
#[command(arg_required_else_help = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    Schema {
        #[arg(short, long)]
        source: String,

        #[arg(short, long)]
        target: String,

        #[arg(long, help = "Table or SQL query for the source (required when source is a database URI)")]
        source_query: Option<String>,

        #[arg(long, help = "Table or SQL query for the target (required when target is a database URI)")]
        target_query: Option<String>,

        #[arg(long, help = "Optional path to a JSON schema policy/contract file")]
        policy: Option<String>,
    },
    Data {
        #[arg(short, long)]
        source: String,

        #[arg(short, long)]
        target: String,

        #[arg(long, help = "Table or SQL query for the source (required when source is a database URI)")]
        source_query: Option<String>,

        #[arg(long, help = "Table or SQL query for the target (required when target is a database URI)")]
        target_query: Option<String>,

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

        #[arg(long, help = "Maximum absolute difference between two numbers before they count as changed (e.g. 0.01 ignores sub-cent differences)")]
        numeric_tolerance: Option<f64>,

        #[arg(
            long,
            conflicts_with = "numeric_tolerance",
            help = "Maximum difference as a percentage of the larger value (e.g. 5 ignores changes under 5%)"
        )]
        numeric_tolerance_percent: Option<f64>,

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

        #[arg(long, help = "Maximum absolute difference between two numbers before they count as changed (e.g. 0.01 ignores sub-cent differences)")]
        numeric_tolerance: Option<f64>,

        #[arg(
            long,
            conflicts_with = "numeric_tolerance",
            help = "Maximum difference as a percentage of the larger value (e.g. 5 ignores changes under 5%)"
        )]
        numeric_tolerance_percent: Option<f64>,

        #[arg(long, help = "Show only per-pair diff counts, skip verbose summaries")]
        diffs_only: bool,

        #[arg(long, help = "Stop the batch as soon as one pair fails")]
        fail_fast: bool,
    },
}

/// Run one of the free commands.
pub fn dispatch(command: Commands) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        Commands::Schema {
            source,
            target,
            source_query,
            target_query,
            policy,
        } => {
            schema::schema_diff(&source, &target, source_query.as_deref(), target_query.as_deref(), policy.as_deref())?;
        }
        Commands::Data {
            source,
            target,
            source_query,
            target_query,
            key,
            output,
            format,
            temp,
            exclude_columns,
            only_columns,
            numeric_tolerance,
            numeric_tolerance_percent,
            diffs_only,
            json,
        } => {
            data::validate_export_args(output.as_deref(), format.as_ref(), temp)?;
            let tolerance = data::Tolerance::resolve(numeric_tolerance, numeric_tolerance_percent)?;
            data::data_diff(
                &source,
                &target,
                &key,
                source_query.as_deref(),
                target_query.as_deref(),
                output.as_deref(),
                format,
                temp,
                exclude_columns.as_deref(),
                only_columns.as_deref(),
                tolerance,
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
            numeric_tolerance_percent,
            diffs_only,
            fail_fast,
        } => {
            data::validate_export_args(output.as_deref(), format.as_ref(), false)?;
            let tolerance = data::Tolerance::resolve(numeric_tolerance, numeric_tolerance_percent)?;
            data::batch_diff(
                &manifest,
                manifest_format,
                &key,
                output.as_deref(),
                format,
                exclude_columns.as_deref(),
                only_columns.as_deref(),
                tolerance,
                diffs_only,
                fail_fast,
            )?;
        }
    }

    Ok(())
}
