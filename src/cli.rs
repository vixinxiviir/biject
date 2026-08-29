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
// README documents `biject --version`, and users expect it regardless.
#[command(version)]
#[command(about = "A CLI tool for diffing data and schemas")]
#[command(arg_required_else_help = true)]
/// The parsed command line.
pub struct Cli {
    /// The subcommand to run.
    #[command(subcommand)]
    pub command: Commands,
}

/// The free commands.
#[derive(Subcommand)]
pub enum Commands {
    /// Compare the structure of two tables or files.
    Schema {
        /// Source table, file or database URI.
        #[arg(short, long)]
        source: String,

        /// Target table, file or database URI.
        #[arg(short, long)]
        target: String,

        /// Table or SQL query for the source, required when the source is a
        /// database URI.
        #[arg(
            long,
            help = "Table or SQL query for the source (required when source is a database URI)"
        )]
        source_query: Option<String>,

        /// Table or SQL query for the target, required when the target is a
        /// database URI.
        #[arg(
            long,
            help = "Table or SQL query for the target (required when target is a database URI)"
        )]
        target_query: Option<String>,

        /// Path to a JSON schema policy to evaluate the comparison against.
        #[arg(long, help = "Optional path to a JSON schema policy/contract file")]
        policy: Option<String>,

        /// File to write the comparison to. Requires `--format`.
        #[arg(long, help = "File to write the comparison to; requires --format")]
        output: Option<String>,

        /// Format for `--output`.
        #[arg(
            long,
            value_enum,
            help = "Export format: json or csv; requires --output"
        )]
        format: Option<data::ExportFormat>,

        /// Exit non-zero when the comparison finds changes at or above this
        /// severity.
        #[arg(
            long,
            value_enum,
            help = "Exit non-zero when the comparison finds changes at or above this severity"
        )]
        fail_on: Option<data::FailOn>,
    },
    /// Compare the rows of two tables or files, matched on a key.
    Data {
        /// Source table, file or database URI.
        #[arg(short, long)]
        source: String,

        /// Target table, file or database URI.
        #[arg(short, long)]
        target: String,

        /// Table or SQL query for the source, required when the source is a
        /// database URI.
        #[arg(
            long,
            help = "Table or SQL query for the source (required when source is a database URI)"
        )]
        source_query: Option<String>,

        /// Table or SQL query for the target, required when the target is a
        /// database URI.
        #[arg(
            long,
            help = "Table or SQL query for the target (required when target is a database URI)"
        )]
        target_query: Option<String>,

        /// Columns used to match rows.
        #[arg(short, long, required = true)]
        key: Vec<String>,

        /// File to write the comparison to; requires --format.
        #[arg(long)]
        output: Option<String>,

        /// Export format: json or csv; requires --output.
        #[arg(long, value_enum)]
        format: Option<data::ExportFormat>,

        /// Write output to a timestamped temporary directory instead of --output.
        #[arg(long)]
        temp: bool,

        /// Columns to leave out of the comparison, comma-separated.
        #[arg(long, help = "Columns to exclude from comparison (comma-separated)")]
        exclude_columns: Option<String>,

        /// The only columns to compare, comma-separated.
        #[arg(long, help = "Only compare these columns (comma-separated)")]
        only_columns: Option<String>,

        /// Largest absolute difference between two numbers that still counts
        /// as equal.
        #[arg(
            long,
            help = "Maximum absolute difference between two numbers before they count as changed (e.g. 0.01 ignores sub-cent differences)"
        )]
        numeric_tolerance: Option<f64>,

        /// The same, as a percentage of the larger value.
        #[arg(
            long,
            conflicts_with = "numeric_tolerance",
            help = "Maximum difference as a percentage of the larger value (e.g. 5 ignores changes under 5%)"
        )]
        numeric_tolerance_percent: Option<f64>,

        /// Print only modified rows, without the summary tables.
        #[arg(long, help = "Show only modified rows, skip summary tables")]
        diffs_only: bool,

        /// Print the result as JSON on stdout, and nothing else.
        #[arg(
            long,
            help = "Output results as JSON to stdout (suppresses all other output)"
        )]
        json: bool,
    },
    /// Run many data comparisons from a manifest.
    Batch {
        /// Path to a manifest describing the source and target pairs to run.
        #[arg(
            long,
            help = "Path to a batch manifest describing source/target pairs (JSON or CSV)"
        )]
        manifest: String,

        /// How to parse the manifest, when its extension does not say.
        #[arg(
            long,
            value_enum,
            help = "Override manifest parsing format (json or csv)"
        )]
        manifest_format: Option<data::ManifestFormat>,

        /// Columns used to match rows.
        #[arg(short, long, required = true)]
        key: Vec<String>,

        /// File to write the comparison to; requires --format.
        #[arg(long)]
        output: Option<String>,

        /// Export format: json or csv; requires --output.
        #[arg(long, value_enum)]
        format: Option<data::ExportFormat>,

        /// Columns to leave out of the comparison, comma-separated.
        #[arg(long, help = "Columns to exclude from comparison (comma-separated)")]
        exclude_columns: Option<String>,

        /// The only columns to compare, comma-separated.
        #[arg(long, help = "Only compare these columns (comma-separated)")]
        only_columns: Option<String>,

        /// Largest absolute difference between two numbers that still counts
        /// as equal.
        #[arg(
            long,
            help = "Maximum absolute difference between two numbers before they count as changed (e.g. 0.01 ignores sub-cent differences)"
        )]
        numeric_tolerance: Option<f64>,

        /// The same, as a percentage of the larger value.
        #[arg(
            long,
            conflicts_with = "numeric_tolerance",
            help = "Maximum difference as a percentage of the larger value (e.g. 5 ignores changes under 5%)"
        )]
        numeric_tolerance_percent: Option<f64>,

        /// Print only the per-pair counts, without the verbose summaries.
        #[arg(long, help = "Show only per-pair diff counts, skip verbose summaries")]
        diffs_only: bool,

        /// Stop at the first pair that fails rather than running the rest.
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
            output,
            format,
            fail_on,
        } => {
            // Same both-or-neither rule the data command enforces.
            data::validate_export_args(output.as_deref(), format.as_ref(), false)?;
            schema::schema_diff(
                &source,
                &target,
                source_query.as_deref(),
                target_query.as_deref(),
                policy.as_deref(),
                output.as_deref(),
                format,
                fail_on,
            )?;
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
