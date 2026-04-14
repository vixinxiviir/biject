use anyhow::{anyhow, Result};
use chrono::Local;
use clap::ValueEnum;
use polars::prelude::*;
use prettytable::{row, Table};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::fs::File;
use std::io::{self, BufRead, BufWriter, Write};
use std::path::{Path, PathBuf};

/// Composite key separator for multi-key rows
const COMPOSITE_KEY_SEP: &str = "::";

#[derive(Clone, Debug, ValueEnum)]
pub enum ExportFormat {
    Csv,
    Json,
}

#[derive(Clone, Debug, ValueEnum)]
pub enum ManifestFormat {
    Json,
    Csv,
}

#[derive(Clone, Debug)]
pub enum DataDiffError {
    CLICommandError(String),
    MissingKeyColumn(String),
    DataContentError(String),
    FileNotFound(String),
    InvalidManifestEntry(String),
    SchemaMismatch(String),
}

impl std::fmt::Display for DataDiffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DataDiffError::CLICommandError(msg) => write!(f, "CLI command error: {}", msg),
            DataDiffError::MissingKeyColumn(col) => {
                write!(f, "Missing key column: {} does not exist in data", col)
            }
            DataDiffError::DataContentError(msg) => write!(f, "Error while processing data: {}", msg),
            DataDiffError::FileNotFound(path) => write!(f, "File not found: {}", path),
            DataDiffError::InvalidManifestEntry(msg) => {
                write!(f, "Invalid manifest entry: {}", msg)
            }
            DataDiffError::SchemaMismatch(msg) => write!(f, "Schema mismatch: {}", msg),
        }
    }
}

impl std::error::Error for DataDiffError {}

impl From<std::io::Error> for DataDiffError {
    fn from(err: std::io::Error) -> Self {
        DataDiffError::FileNotFound(err.to_string())
    }
}

impl From<polars::error::PolarsError> for DataDiffError {
    fn from(err: polars::error::PolarsError) -> Self {
        DataDiffError::DataContentError(err.to_string())
    }
}

impl From<serde_json::Error> for DataDiffError {
    fn from(err: serde_json::Error) -> Self {
        DataDiffError::InvalidManifestEntry(err.to_string())
    }
}

impl From<anyhow::Error> for DataDiffError {
    fn from(err: anyhow::Error) -> Self {
        DataDiffError::DataContentError(err.to_string())
    }
}

#[derive(Debug)]
struct ColumnFilterSet {
    exclude: HashSet<String>,
    only: HashSet<String>,
}

impl ColumnFilterSet {
    fn new(exclude: Option<&str>, only: Option<&str>) -> Self {
    ColumnFilterSet {
        exclude: parse_column_list(exclude),
        only: parse_column_list(only),
    }
}

    fn should_include(&self, col_name: &str, keys: &[String]) -> bool {
    if keys.contains(&col_name.to_string()) { return false; }
    if !self.only.is_empty() { return self.only.contains(col_name); }
    if !self.exclude.is_empty() { return !self.exclude.contains(col_name); }
    true
}
}
#[derive(Clone, Debug, Serialize)]
struct RowSummary {
    source_rows: usize,
    target_rows: usize,
    target_only_rows: usize,
    target_only_percent: f64,
    source_only_rows: usize,
    source_only_percent: f64,
    modified_rows: usize,
    modified_percent: f64,
}

#[derive(Clone, Debug, Serialize)]
struct ColumnPresenceSummary {
    added_in_target: Vec<String>,
    removed_from_source: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct ColumnStats {
    column: String,
    data_type: String,
    null_count: usize,
    unique_count: usize,
    min: Option<f64>,
    max: Option<f64>,
    mean: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
struct ChangedColumnSummary {
    column: String,
    changed_rows: usize,
    percent_of_changed_rows: f64,
}

#[derive(Clone, Debug, Serialize)]
struct ColumnSummaryExport {
    source: Vec<ColumnStats>,
    target: Vec<ColumnStats>,
    column_presence: ColumnPresenceSummary,
}

#[derive(Clone, Debug, Serialize)]
struct DiffExport {
    key_columns: Vec<String>,
    source_only: Vec<String>,
    target_only: Vec<String>,
    modified: Vec<String>,
    row_summary: RowSummary,
    column_summary: ColumnSummaryExport,
    change_summary: Vec<ChangedColumnSummary>,
}

#[derive(Debug, Deserialize)]
struct BatchManifestEntry {
    name: Option<String>,
    source: String,
    target: String,
    key: Option<String>,
    output_base: Option<String>,
    exclude_columns: Option<String>,
    only_columns: Option<String>,
    numeric_tolerance: Option<f64>,
    diffs_only: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct BatchCsvManifestEntry {
    name: Option<String>,
    source: String,
    target: String,
    key: Option<String>,
    output_base: Option<String>,
    exclude_columns: Option<String>,
    only_columns: Option<String>,
    numeric_tolerance: Option<f64>,
    diffs_only: Option<bool>,
}

#[derive(Clone, Debug, Serialize)]
struct BatchPairResult {
    name: String,
    source: String,
    target: String,
    status: String,
    source_only_rows: usize,
    target_only_rows: usize,
    modified_rows: usize,
    source_rows: usize,
    target_rows: usize,
    added_columns: usize,
    removed_columns: usize,
    error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct AggregatedChangedColumn {
    column: String,
    changed_rows: usize,
}

#[derive(Clone, Debug, Serialize)]
struct BatchSummary {
    total_pairs: usize,
    succeeded_pairs: usize,
    failed_pairs: usize,
    total_source_rows: usize,
    total_target_rows: usize,
    total_source_only_rows: usize,
    total_target_only_rows: usize,
    total_modified_rows: usize,
    total_added_columns: usize,
    total_removed_columns: usize,
    top_changed_columns: Vec<AggregatedChangedColumn>,
}

#[derive(Clone, Debug, Serialize)]
struct BatchExport {
    manifest_path: String,
    key_columns: Vec<String>,
    summary: BatchSummary,
    pair_results: Vec<BatchPairResult>,
}

struct DiffComputationOptions<'a> {
    exclude_columns: Option<&'a str>,
    only_columns: Option<&'a str>,
    numeric_tolerance: Option<f64>,
    include_column_stats: bool,
}

/// Build a composite key column for efficient Polars-based operations
/// Concatenates multiple key columns with COMPOSITE_KEY_SEP as a single column
fn build_composite_key_column(df: &DataFrame, keys: &[String]) -> Result<Series> {
    if keys.is_empty() {
        return Err(anyhow!("No keys specified for composite key"));
    }

     // Build composite keys by efficiently pooling row data
     let height = df.height();
     let mut composite_keys: Vec<String> = Vec::with_capacity(height);
 
     for row_idx in 0..height {
         let key_parts: Result<Vec<String>> = keys
             .iter()
             .map(|key| {
                 let col = df.column(key)?;
                 let val = col.get(row_idx)?;
                 Ok(val.to_string())
             })
             .collect();
         composite_keys.push(key_parts?.join(COMPOSITE_KEY_SEP));
     }
 
     Ok(Series::new("__keys__", composite_keys))
}
 
/// Build a HashMap of composite keys to row indices
fn build_composite_key_map(df: &DataFrame, keys: &[String]) -> Result<HashMap<String, usize>> {
    let mut map = HashMap::with_capacity(df.height());
    let key_series = build_composite_key_column(df, keys)?;
    if let Ok(key_str) = key_series.str() {
        for (idx, opt_key) in key_str.iter().enumerate() {
            if let Some(key_val) = opt_key {
                map.insert(key_val.to_string(), idx);
            }
        }
    }
    Ok(map)
}

/// Parse a comma-separated column list into a HashSet
fn parse_column_list(columns: Option<&str>) -> HashSet<String> {
    match columns {
        Some(s) if !s.is_empty() => s.split(',').map(|c| c.trim().to_string()).collect(),
        _ => HashSet::new(),
    }
}


fn parse_manifest_keys(raw_keys: &str) -> Result<Vec<String>, DataDiffError> {
    let parsed: Vec<String> = raw_keys
        .split(',')
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
        .collect();

    if parsed.is_empty() {
        return Err(DataDiffError::InvalidManifestEntry(
            "Batch manifest entry has an empty key override".to_string(),
        ));
    }

    Ok(parsed)
}

fn anyvalue_to_f64(value: &polars::prelude::AnyValue<'_>) -> Option<f64> {
    use polars::prelude::AnyValue;

    match value {
        AnyValue::Int8(v) => Some(*v as f64),
        AnyValue::Int16(v) => Some(*v as f64),
        AnyValue::Int32(v) => Some(*v as f64),
        AnyValue::Int64(v) => Some(*v as f64),
        AnyValue::UInt8(v) => Some(*v as f64),
        AnyValue::UInt16(v) => Some(*v as f64),
        AnyValue::UInt32(v) => Some(*v as f64),
        AnyValue::UInt64(v) => Some(*v as f64),
        AnyValue::Float32(v) => Some(*v as f64),
        AnyValue::Float64(v) => Some(*v),
        _ => None,
    }
}



/// Compare two values with optional numeric tolerance
fn values_equal(left: &polars::prelude::AnyValue, right: &polars::prelude::AnyValue, tolerance: Option<f64>) -> bool {
    if let Some(tol) = tolerance {
        if let (Some(left_num), Some(right_num)) = (anyvalue_to_f64(left), anyvalue_to_f64(right)) {
            return (left_num - right_num).abs() <= tol;
        }
    }

    left == right
}

pub fn data_diff(
    path1: &str,
    path2: &str,
    keys: &[String],
    output: Option<&str>,
    format: Option<ExportFormat>,
    temp: bool,
    exclude_columns: Option<&str>,
    only_columns: Option<&str>,
    numeric_tolerance: Option<f64>,
    diffs_only: bool,
    json_output: bool,
) -> Result<()> {
    let options = DiffComputationOptions {
        exclude_columns,
        only_columns,
        numeric_tolerance,
        include_column_stats: !diffs_only || !temp || output.is_some() || format.is_some(),
    };

    let export_payload = compute_diff_export(path1, path2, keys, &options)?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&export_payload)?);
        return Ok(());
    }

    render_diff_report(path1, path2, keys, &export_payload, diffs_only);

    if temp {
        return Ok(());
    }

    if let (Some(output_path), Some(export_format)) = (output, format) {
        let export_folder = create_export_folder()?;
        let export_base = export_path_in_folder(&export_folder, output_path);
        export_diff(export_base.to_str().unwrap(), export_format, &export_payload)?;
        println!("\nExported results to: {}", export_folder.display());
    } else if let Some((prompt_path, prompt_format)) = prompt_for_export(path1, path2)? {
        let export_folder = create_export_folder()?;
        let export_base = export_path_in_folder(&export_folder, &prompt_path);
        export_diff(export_base.to_str().unwrap(), prompt_format, &export_payload)?;
        println!("\nExported results to: {}", export_folder.display());
    }

    Ok(())
}

pub fn batch_diff(
    manifest_path: &str,
    manifest_format: Option<ManifestFormat>,
    keys: &[String],
    output: Option<&str>,
    format: Option<ExportFormat>,
    exclude_columns: Option<&str>,
    only_columns: Option<&str>,
    numeric_tolerance: Option<f64>,
    diffs_only: bool,
    fail_fast: bool,
) -> Result<()> {
    if keys.is_empty() {
        return Err(anyhow!("At least one key column must be specified"));
    }

    let manifest_entries = read_batch_manifest(manifest_path, manifest_format)?;
    if manifest_entries.is_empty() {
        return Err(anyhow!("Batch manifest does not contain any source/target pairs"));
    }

    let export_folder = if output.is_some() && format.is_some() {
        Some(create_export_folder()?)
    } else {
        None
    };

    if let Some(folder) = &export_folder {
        fs::create_dir_all(folder.join("pairs"))?;
    }

    let mut pair_results = Vec::with_capacity(manifest_entries.len());
    let mut aggregated_columns: HashMap<String, usize> = HashMap::new();

    for entry in manifest_entries {
        let pair_name = batch_pair_name(&entry);
        let pair_keys = match entry.key.as_deref() {
            Some(raw_keys) => parse_manifest_keys(raw_keys)?,
            None => keys.to_vec(),
        };
        let pair_diffs_only = entry.diffs_only.unwrap_or(diffs_only);
        let pair_options = DiffComputationOptions {
            exclude_columns: entry.exclude_columns.as_deref().or(exclude_columns),
            only_columns: entry.only_columns.as_deref().or(only_columns),
            numeric_tolerance: entry.numeric_tolerance.or(numeric_tolerance),
            include_column_stats: output.is_some() || format.is_some() || !pair_diffs_only,
        };

        match compute_diff_export(&entry.source, &entry.target, &pair_keys, &pair_options) {
            Ok(export_payload) => {
                for changed_column in &export_payload.change_summary {
                    *aggregated_columns
                        .entry(changed_column.column.clone())
                        .or_insert(0) += changed_column.changed_rows;
                }

                if let (Some(export_format), Some(folder), Some(output_base)) =
                    (format.as_ref(), export_folder.as_ref(), output)
                {
                    let pair_output_base = entry
                        .output_base
                        .as_deref()
                        .map(sanitize_file_component)
                        .filter(|value| !value.is_empty())
                        .unwrap_or_else(|| sanitize_file_component(&format!("{}_{}", output_base, pair_name)));
                    let pair_base = folder
                        .join("pairs")
                        .join(pair_output_base);
                    export_diff(pair_base.to_str().unwrap(), export_format.clone(), &export_payload)?;
                }

                pair_results.push(BatchPairResult {
                    name: pair_name,
                    source: entry.source,
                    target: entry.target,
                    status: "ok".to_string(),
                    source_only_rows: export_payload.row_summary.source_only_rows,
                    target_only_rows: export_payload.row_summary.target_only_rows,
                    modified_rows: export_payload.row_summary.modified_rows,
                    source_rows: export_payload.row_summary.source_rows,
                    target_rows: export_payload.row_summary.target_rows,
                    added_columns: export_payload.column_summary.column_presence.added_in_target.len(),
                    removed_columns: export_payload.column_summary.column_presence.removed_from_source.len(),
                    error: None,
                });
            }
            Err(error) => {
                pair_results.push(BatchPairResult {
                    name: pair_name,
                    source: entry.source,
                    target: entry.target,
                    status: "failed".to_string(),
                    source_only_rows: 0,
                    target_only_rows: 0,
                    modified_rows: 0,
                    source_rows: 0,
                    target_rows: 0,
                    added_columns: 0,
                    removed_columns: 0,
                    error: Some(error.to_string()),
                });

                if fail_fast {
                    break;
                }
            }
        }
    }

    let batch_summary = build_batch_summary(&pair_results, aggregated_columns);
    print_batch_pair_summary(&pair_results, diffs_only);
    print_batch_run_summary(&batch_summary);

    if let (Some(output_base), Some(export_format), Some(folder)) = (output, format, export_folder.as_ref()) {
        let export_base = export_path_in_folder(folder, output_base);
        let batch_export = BatchExport {
            manifest_path: manifest_path.to_string(),
            key_columns: keys.to_vec(),
            summary: batch_summary,
            pair_results,
        };
        export_batch(export_base.to_str().unwrap(), export_format, &batch_export)?;
        println!("\nExported batch results to: {}", folder.display());
    }

    Ok(())
}

fn compute_diff_export(
    path1: &str,
    path2: &str,
    keys: &[String],
    options: &DiffComputationOptions<'_>,
) -> Result<DiffExport, DataDiffError> {
    let df1 = CsvReader::from_path(path1)?
        .infer_schema(Some(100))
        .has_header(true)
        .finish()?;

    let df2 = CsvReader::from_path(path2)?
        .infer_schema(Some(100))
        .has_header(true)
        .finish()?;

    diff_dataframes(df1, df2, path1, path2, keys, options)
}

/// Core diff logic that operates on already-loaded DataFrames.
/// `source_label` and `target_label` are used in error messages only.
fn diff_dataframes(
    df1: DataFrame,
    df2: DataFrame,
    source_label: &str,
    target_label: &str,
    keys: &[String],
    options: &DiffComputationOptions<'_>,
) -> Result<DiffExport, DataDiffError> {
    // Build filter set — single allocation, validated once
    if options.exclude_columns.is_some() && options.only_columns.is_some() {
        return Err(DataDiffError::CLICommandError(
            "Cannot use both --exclude-columns and --only-columns".to_string(),
        ));
    }
    let filters = ColumnFilterSet::new(options.exclude_columns, options.only_columns);

    // Validate that all key columns exist in both dataframes
    for key in keys {
        if !df1.get_column_names().contains(&key.as_str()) {
            return Err(DataDiffError::MissingKeyColumn(format!("Key column '{}' not found in source: {}", key, source_label)));
        }
        if !df2.get_column_names().contains(&key.as_str()) {
            return Err(DataDiffError::MissingKeyColumn(format!("Key column '{}' not found in target: {}", key, target_label)));
        }
    }

     // Build row index maps using composite keys (optimized with HashMap capacity pre-allocation)
     let map1: HashMap<String, usize> = build_composite_key_map(&df1, keys)?;
     let map2: HashMap<String, usize> = build_composite_key_map(&df2, keys)?;
 
     // Use iterators for efficient set operations without cloning all keys
     let keys1: HashSet<String> = map1.keys().cloned().collect();
     let keys2: HashSet<String> = map2.keys().cloned().collect();
 
     let mut target_only: Vec<String> = keys2.difference(&keys1).cloned().collect();
     let mut source_only: Vec<String> = keys1.difference(&keys2).cloned().collect();
     target_only.sort();
     source_only.sort();

    let cols1: HashSet<String> = df1
        .get_column_names()
        .iter()
        .map(|name| name.to_string())
        .collect();
    let cols2: HashSet<String> = df2
        .get_column_names()
        .iter()
        .map(|name| name.to_string())
        .collect();

    let mut added_columns: Vec<String> = cols2.difference(&cols1).cloned().collect();
    let mut removed_columns: Vec<String> = cols1.difference(&cols2).cloned().collect();
    added_columns.sort();
    removed_columns.sort();

    // Restrict row comparisons to the shared non-key columns so schema changes
    // remain the responsibility of schema_diff while data_diff focuses on row changes.
    // Also apply user's column filters (exclude/only).
    let mut comparable_columns: Vec<String> = cols1
        .intersection(&cols2)
        .filter(|name| filters.should_include(name, keys))
        .cloned()
        .collect();
    comparable_columns.sort();

     // Optimize: Pre-allocate with estimated capacity for modified rows (~10% of shared)
     let shared_keys: HashSet<String> = keys1.intersection(&keys2).cloned().collect();
     let shared_keys_count = shared_keys.len();
     let mut modified: Vec<String> = Vec::with_capacity(shared_keys_count / 10);
     let mut changed_column_counts: HashMap<String, usize> = comparable_columns
         .iter()
         .cloned()
         .map(|column| (column, 0usize))
         .collect();
 
     // Optimized loop: only compare shared rows using iterator intersection
     for key_value in &shared_keys {
         let left_idx = map1[key_value];
         let right_idx = map2[key_value];
         let mut row_changed = false;
 
         for column in &comparable_columns {
             let left_value = df1.column(column)?.get(left_idx).unwrap();
             let right_value = df2.column(column)?.get(right_idx).unwrap();
 
             if !values_equal(&left_value, &right_value, options.numeric_tolerance) {
                 row_changed = true;
                 *changed_column_counts.get_mut(column).unwrap() += 1;
             }
         }
 
         if row_changed {
             modified.push(key_value.clone());
         }
     }
     modified.sort();

    let shared_keys_count = keys1.len() - source_only.len();  // Shared rows = total in source - source-only
    let row_summary = build_row_summary(df1.height(), df2.height(), target_only.len(), source_only.len(), modified.len(), shared_keys_count);
    let column_presence = ColumnPresenceSummary {
        added_in_target: added_columns.clone(),
        removed_from_source: removed_columns.clone(),
    };
    let source_column_summary = if options.include_column_stats {
        build_column_stats(&df1)?
    } else {
        Vec::new()
    };
    let target_column_summary = if options.include_column_stats {
        build_column_stats(&df2)?
    } else {
        Vec::new()
    };
    let change_summary = build_change_summary(&comparable_columns, &changed_column_counts, modified.len());

    Ok(DiffExport {
        key_columns: keys.to_vec(),
        source_only: source_only.clone(),
        target_only: target_only.clone(),
        modified: modified.clone(),
        row_summary,
        column_summary: ColumnSummaryExport {
            source: source_column_summary,
            target: target_column_summary,
            column_presence,
        },
        change_summary,
    })
}

fn build_row_summary(
    source_rows: usize,
    target_rows: usize,
    target_only_rows: usize,
    source_only_rows: usize,
    modified_rows: usize,
    shared_rows: usize,
) -> RowSummary {
    // Percentages are calculated against the relevant row universe so the
    // export and CLI summaries describe the diff from a useful baseline.
    RowSummary {
        source_rows,
        target_rows,
        target_only_rows,
        target_only_percent: percentage(target_only_rows, target_rows),
        source_only_rows,
        source_only_percent: percentage(source_only_rows, source_rows),
        modified_rows,
        modified_percent: percentage(modified_rows, shared_rows),
    }
}

fn build_column_stats(df: &DataFrame) -> Result<Vec<ColumnStats>> {
    let mut stats = Vec::new();

    for column_name in df.get_column_names() {
        let series = df.column(column_name)?;
        let dtype = series.dtype();

        // Null and unique counts apply to every column regardless of type.
        let null_count = series.null_count();
        let unique_count = series.n_unique()?;

        // Numeric min/max/mean are only computed for numeric columns. Other
        // types keep None so the exporter emits null and the CLI prints "-".
        let (min, max, mean) = if is_numeric(dtype) {
            let casted = series.cast(&DataType::Float64)?;
            let values = casted.f64()?;
            (values.min(), values.max(), values.mean())
        } else {
            (None, None, None)
        };

        stats.push(ColumnStats {
            column: column_name.to_string(),
            data_type: format!("{dtype:?}"),
            null_count,
            unique_count,
            min,
            max,
            mean,
        });
    }

    Ok(stats)
}

fn build_change_summary(
    comparable_columns: &[String],
    changed_column_counts: &HashMap<String, usize>,
    changed_rows: usize,
) -> Vec<ChangedColumnSummary> {
    comparable_columns
        .iter()
        .map(|column| ChangedColumnSummary {
            column: column.clone(),
            changed_rows: *changed_column_counts.get(column).unwrap_or(&0),
            percent_of_changed_rows: percentage(*changed_column_counts.get(column).unwrap_or(&0), changed_rows),
        })
        .collect()
}

fn render_diff_report(path1: &str, path2: &str, keys: &[String], payload: &DiffExport, diffs_only: bool) {
    if !diffs_only {
        print_row_summary(path1, path2, &payload.row_summary);
        print_column_presence_summary(path1, path2, &payload.column_summary.column_presence);
        if !payload.column_summary.source.is_empty() {
            print_column_summary(path1, &payload.column_summary.source);
        }
        if !payload.column_summary.target.is_empty() {
            print_column_summary(path2, &payload.column_summary.target);
        }
        print_change_summary(&payload.change_summary);
    }

    print_key_table("Rows only in target", Some(path2), keys, &payload.target_only);
    print_key_table("Rows only in source", Some(path1), keys, &payload.source_only);
    print_key_table("Modified rows", None, keys, &payload.modified);
}

fn read_batch_manifest(path: &str, manifest_format: Option<ManifestFormat>) -> Result<Vec<BatchManifestEntry>> {
    let inferred_format = Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    let resolved_format = manifest_format.unwrap_or_else(|| {
        if inferred_format == "csv" {
            ManifestFormat::Csv
        } else {
            ManifestFormat::Json
        }
    });

    match resolved_format {
        ManifestFormat::Csv => read_batch_manifest_csv(path),
        ManifestFormat::Json => {
            let raw = fs::read_to_string(path)?;
            let normalized = raw.trim_start_matches('\u{feff}').trim();
            let entries: Vec<BatchManifestEntry> = serde_json::from_str(normalized)?;
            Ok(entries)
        }
    }
}

fn read_batch_manifest_csv(path: &str) -> Result<Vec<BatchManifestEntry>> {
    let mut reader = csv::ReaderBuilder::new().trim(csv::Trim::All).from_path(path)?;
    let mut entries = Vec::new();

    for row in reader.deserialize::<BatchCsvManifestEntry>() {
        let row = row?;
        entries.push(BatchManifestEntry {
            name: row.name,
            source: row.source,
            target: row.target,
            key: row.key,
            output_base: row.output_base,
            exclude_columns: row.exclude_columns,
            only_columns: row.only_columns,
            numeric_tolerance: row.numeric_tolerance,
            diffs_only: row.diffs_only,
        });
    }

    Ok(entries)
}

fn batch_pair_name(entry: &BatchManifestEntry) -> String {
    entry
        .name
        .clone()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| {
            let source = Path::new(&entry.source)
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("source");
            let target = Path::new(&entry.target)
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("target");
            format!("{}_vs_{}", source, target)
        })
}

fn sanitize_file_component(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => ch,
            _ => '_',
        })
        .collect();

    sanitized.trim_matches('_').to_string()
}

fn build_batch_summary(
    pair_results: &[BatchPairResult],
    aggregated_columns: HashMap<String, usize>,
) -> BatchSummary {
    let succeeded_pairs = pair_results.iter().filter(|result| result.status == "ok").count();
    let failed_pairs = pair_results.len().saturating_sub(succeeded_pairs);

    let mut top_changed_columns: Vec<AggregatedChangedColumn> = aggregated_columns
        .into_iter()
        .filter(|(_, changed_rows)| *changed_rows > 0)
        .map(|(column, changed_rows)| AggregatedChangedColumn { column, changed_rows })
        .collect();
    top_changed_columns.sort_by(|left, right| {
        right
            .changed_rows
            .cmp(&left.changed_rows)
            .then_with(|| left.column.cmp(&right.column))
    });
    top_changed_columns.truncate(10);

    BatchSummary {
        total_pairs: pair_results.len(),
        succeeded_pairs,
        failed_pairs,
        total_source_rows: pair_results.iter().map(|result| result.source_rows).sum(),
        total_target_rows: pair_results.iter().map(|result| result.target_rows).sum(),
        total_source_only_rows: pair_results.iter().map(|result| result.source_only_rows).sum(),
        total_target_only_rows: pair_results.iter().map(|result| result.target_only_rows).sum(),
        total_modified_rows: pair_results.iter().map(|result| result.modified_rows).sum(),
        total_added_columns: pair_results.iter().map(|result| result.added_columns).sum(),
        total_removed_columns: pair_results.iter().map(|result| result.removed_columns).sum(),
        top_changed_columns,
    }
}

fn print_batch_pair_summary(pair_results: &[BatchPairResult], diffs_only: bool) {
    println!("\nBatch pair summary");
    let mut table = Table::new();
    table.add_row(row![
        "Pair",
        "Status",
        "Source Rows",
        "Target Rows",
        "Source Only",
        "Target Only",
        "Modified"
    ]);

    for result in pair_results {
        table.add_row(row![
            result.name,
            result.status,
            result.source_rows,
            result.target_rows,
            result.source_only_rows,
            result.target_only_rows,
            result.modified_rows
        ]);
    }

    table.printstd();

    if !diffs_only {
        let failures: Vec<&BatchPairResult> = pair_results
            .iter()
            .filter(|result| result.error.is_some())
            .collect();

        if !failures.is_empty() {
            println!("\nBatch failures");
            let mut failure_table = Table::new();
            failure_table.add_row(row!["Pair", "Source", "Target", "Error"]);
            for failure in failures {
                failure_table.add_row(row![
                    failure.name,
                    failure.source,
                    failure.target,
                    failure.error.clone().unwrap_or_default()
                ]);
            }
            failure_table.printstd();
        }
    }
}

fn print_batch_run_summary(summary: &BatchSummary) {
    println!("\nBatch aggregate summary");
    let mut table = Table::new();
    table.add_row(row!["Metric", "Value"]);
    table.add_row(row!["Total pairs", summary.total_pairs]);
    table.add_row(row!["Succeeded pairs", summary.succeeded_pairs]);
    table.add_row(row!["Failed pairs", summary.failed_pairs]);
    table.add_row(row!["Total source rows", summary.total_source_rows]);
    table.add_row(row!["Total target rows", summary.total_target_rows]);
    table.add_row(row!["Total source-only rows", summary.total_source_only_rows]);
    table.add_row(row!["Total target-only rows", summary.total_target_only_rows]);
    table.add_row(row!["Total modified rows", summary.total_modified_rows]);
    table.add_row(row!["Total added columns", summary.total_added_columns]);
    table.add_row(row!["Total removed columns", summary.total_removed_columns]);
    table.printstd();

    if !summary.top_changed_columns.is_empty() {
        println!("\nTop changed columns across batch");
        let mut top_table = Table::new();
        top_table.add_row(row!["Column", "Changed Rows"]);
        for entry in &summary.top_changed_columns {
            top_table.add_row(row![entry.column, entry.changed_rows]);
        }
        top_table.printstd();
    }
}

fn print_row_summary(path1: &str, path2: &str, row_summary: &RowSummary) {
    println!("\nRow-level summary");
    let mut table = Table::new();
    table.add_row(row!["Metric", path1, path2, "Percent"]);
    table.add_row(row!["Total rows", row_summary.source_rows, row_summary.target_rows, "-"]);
    table.add_row(row![
        "Rows only in target",
        "-",
        row_summary.target_only_rows,
        format!("{:.1}%", row_summary.target_only_percent)
    ]);
    table.add_row(row![
        "Rows only in source",
        row_summary.source_only_rows,
        "-",
        format!("{:.1}%", row_summary.source_only_percent)
    ]);
    table.add_row(row![
        "Modified rows",
        row_summary.modified_rows,
        row_summary.modified_rows,
        format!("{:.1}%", row_summary.modified_percent)
    ]);
    table.printstd();
}

fn print_column_presence_summary(path1: &str, path2: &str, summary: &ColumnPresenceSummary) {
    println!("\nColumn presence summary");
    let mut table = Table::new();
    table.add_row(row!["Change Type", "Columns", "Count"]);
    table.add_row(row![
        format!("Added in {path2}"),
        joined_or_dash(&summary.added_in_target),
        summary.added_in_target.len()
    ]);
    table.add_row(row![
        format!("Removed from {path1}"),
        joined_or_dash(&summary.removed_from_source),
        summary.removed_from_source.len()
    ]);
    table.printstd();
}

fn print_column_summary(label: &str, column_stats: &[ColumnStats]) {
    println!("\nColumn-level summary ({label})");

    let mut table = Table::new();
    table.add_row(row![
        "Column",
        "Type",
        "Nulls",
        "Unique",
        "Min",
        "Max",
        "Mean"
    ]);

    for stats in column_stats {
        table.add_row(row![
            stats.column,
            stats.data_type,
            stats.null_count,
            stats.unique_count,
            format_opt_f64(stats.min),
            format_opt_f64(stats.max),
            format_opt_f64(stats.mean)
        ]);
    }

    table.printstd();
}

fn print_change_summary(change_summary: &[ChangedColumnSummary]) {
    println!("\nChanged-columns summary");
    let mut table = Table::new();
    table.add_row(row!["Column", "Changed Rows", "Percent of Changed Rows"]);

    for entry in change_summary {
        table.add_row(row![
            entry.column,
            entry.changed_rows,
            format!("{:.1}%", entry.percent_of_changed_rows)
        ]);
    }

    table.printstd();
}

fn print_key_table(title: &str, dataset_label: Option<&str>, keys: &[String], key_values: &[String]) {
    if key_values.is_empty() {
        return;
    }

    let mut table = Table::new();
    // Add header with all key columns
    let header_cells: Vec<&str> = keys.iter().map(|k| k.as_str()).collect();
    table.add_row(row![header_cells.join(" | ")]);
    
    // Add rows with composite key values (split by separator for display)
    for composite_key in key_values {
        let parts: Vec<&str> = composite_key.split(COMPOSITE_KEY_SEP).collect();
        table.add_row(row![parts.join(" | ")]);
    }

    match dataset_label {
        Some(label) => println!("\n{title} ({label})"),
        None => println!("\n{title}"),
    }

    table.printstd();
}

fn export_diff(output_path: &str, format: ExportFormat, payload: &DiffExport) -> Result<()> {
    match format {
        ExportFormat::Json => export_json(output_path, payload),
        ExportFormat::Csv => export_csv(output_path, payload),
    }
}

fn export_batch(output_path: &str, format: ExportFormat, payload: &BatchExport) -> Result<()> {
    match format {
        ExportFormat::Json => export_batch_json(output_path, payload),
        ExportFormat::Csv => export_batch_csv(output_path, payload),
    }
}

/// Create a timestamped export folder in the current directory
fn create_export_folder() -> Result<PathBuf> {
    let timestamp = Local::now().format("%Y-%m-%d_%H%M%S").to_string();
    let folder_name = format!("outputs-{}", timestamp);
    let folder_path = Path::new(&folder_name);
    
    fs::create_dir_all(folder_path)?;
    Ok(folder_path.to_path_buf())
}

/// Get the export path within a folder (creates folder if needed)
fn export_path_in_folder(folder_path: &Path, base_name: &str) -> PathBuf {
    let base = if base_name.is_empty() {
        "datadiff_export"
    } else {
        base_name
    };
    folder_path.join(base)
}

fn export_json(output_path: &str, payload: &DiffExport) -> Result<()> {
    let file = File::create(output_path)?;
    let writer = BufWriter::new(file);
    serde_json::to_writer_pretty(writer, payload)?;
    Ok(())
}

fn export_batch_json(output_path: &str, payload: &BatchExport) -> Result<()> {
    let file = File::create(output_path)?;
    let writer = BufWriter::new(file);
    serde_json::to_writer_pretty(writer, payload)?;
    Ok(())
}

fn export_csv(output_path: &str, payload: &DiffExport) -> Result<()> {
    let base_path = Path::new(output_path);

    // CSV export uses multiple files so each dataset can keep a single header
    // and flat schema, which is easier to consume downstream than mixed sections.
    write_key_csv(&csv_output_path(base_path, "target_only"), &payload.key_columns, &payload.target_only)?;
    write_key_csv(&csv_output_path(base_path, "source_only"), &payload.key_columns, &payload.source_only)?;
    write_key_csv(&csv_output_path(base_path, "modified"), &payload.key_columns, &payload.modified)?;
    write_row_summary_csv(&csv_output_path(base_path, "row_summary"), &payload.row_summary)?;
    write_column_stats_csv(
        &csv_output_path(base_path, "column_summary_source"),
        "source",
        &payload.column_summary.source,
    )?;
    write_column_stats_csv(
        &csv_output_path(base_path, "column_summary_target"),
        "target",
        &payload.column_summary.target,
    )?;
    write_column_presence_csv(
        &csv_output_path(base_path, "column_presence"),
        &payload.column_summary.column_presence,
    )?;
    write_change_summary_csv(&csv_output_path(base_path, "change_summary"), &payload.change_summary)?;

    Ok(())
}

fn export_batch_csv(output_path: &str, payload: &BatchExport) -> Result<()> {
    let base_path = Path::new(output_path);
    write_batch_summary_csv(&csv_output_path(base_path, "batch_summary"), &payload.summary)?;
    write_batch_pair_results_csv(&csv_output_path(base_path, "batch_pairs"), &payload.pair_results)?;
    write_batch_top_columns_csv(
        &csv_output_path(base_path, "batch_top_changed_columns"),
        &payload.summary.top_changed_columns,
    )?;
    Ok(())
}

fn csv_output_path(base_path: &Path, suffix: &str) -> PathBuf {
    let parent = base_path.parent().unwrap_or_else(|| Path::new(""));
    let stem = base_path
        .file_stem()
        .or_else(|| base_path.file_name())
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("datadiff_export");

    parent.join(format!("{stem}_{suffix}.csv"))
}

fn write_key_csv(path: &Path, key_columns: &[String], key_values: &[String]) -> Result<()> {
    let mut writer = csv_writer(path)?;
    // Write header with all key column names
    writeln!(writer, "{}", key_columns.iter().map(|k| csv_escape(k)).collect::<Vec<_>>().join(","))?;
    // Write rows with composite key values (split by separator for display)
    for composite_key in key_values {
        let parts: Vec<&str> = composite_key.split(COMPOSITE_KEY_SEP).collect();
        writeln!(writer, "{}", parts.iter().map(|p| csv_escape(p)).collect::<Vec<_>>().join(","))?;
    }
    Ok(())
}

fn write_row_summary_csv(path: &Path, row_summary: &RowSummary) -> Result<()> {
    let mut writer = csv_writer(path)?;
    writeln!(writer, "metric,value")?;
    writeln!(writer, "source_rows,{}", row_summary.source_rows)?;
    writeln!(writer, "target_rows,{}", row_summary.target_rows)?;
    writeln!(writer, "target_only_rows,{}", row_summary.target_only_rows)?;
    writeln!(
        writer,
        "target_only_percent,{:.3}",
        row_summary.target_only_percent
    )?;
    writeln!(writer, "source_only_rows,{}", row_summary.source_only_rows)?;
    writeln!(
        writer,
        "source_only_percent,{:.3}",
        row_summary.source_only_percent
    )?;
    writeln!(writer, "modified_rows,{}", row_summary.modified_rows)?;
    writeln!(
        writer,
        "modified_percent,{:.3}",
        row_summary.modified_percent
    )?;
    Ok(())
}

fn write_column_stats_csv(path: &Path, dataset: &str, stats: &[ColumnStats]) -> Result<()> {
    let mut writer = csv_writer(path)?;
    writeln!(writer, "dataset,column,data_type,null_count,unique_count,min,max,mean")?;
    for entry in stats {
        writeln!(
            writer,
            "{},{},{},{},{},{},{},{}",
            csv_escape(dataset),
            csv_escape(&entry.column),
            csv_escape(&entry.data_type),
            entry.null_count,
            entry.unique_count,
            format_csv_opt_f64(entry.min),
            format_csv_opt_f64(entry.max),
            format_csv_opt_f64(entry.mean)
        )?;
    }
    Ok(())
}

fn write_column_presence_csv(path: &Path, summary: &ColumnPresenceSummary) -> Result<()> {
    let mut writer = csv_writer(path)?;
    writeln!(writer, "change_type,column_name")?;
    for column in &summary.added_in_target {
        writeln!(writer, "added_in_target,{}", csv_escape(column))?;
    }
    for column in &summary.removed_from_source {
        writeln!(writer, "removed_from_source,{}", csv_escape(column))?;
    }
    Ok(())
}

fn write_change_summary_csv(path: &Path, change_summary: &[ChangedColumnSummary]) -> Result<()> {
    let mut writer = csv_writer(path)?;
    writeln!(writer, "column,changed_rows,percent_of_changed_rows")?;
    for entry in change_summary {
        writeln!(
            writer,
            "{},{},{:.3}",
            csv_escape(&entry.column),
            entry.changed_rows,
            entry.percent_of_changed_rows
        )?;
    }
    Ok(())
}

fn write_batch_summary_csv(path: &Path, summary: &BatchSummary) -> Result<()> {
    let mut writer = csv_writer(path)?;
    writeln!(writer, "metric,value")?;
    writeln!(writer, "total_pairs,{}", summary.total_pairs)?;
    writeln!(writer, "succeeded_pairs,{}", summary.succeeded_pairs)?;
    writeln!(writer, "failed_pairs,{}", summary.failed_pairs)?;
    writeln!(writer, "total_source_rows,{}", summary.total_source_rows)?;
    writeln!(writer, "total_target_rows,{}", summary.total_target_rows)?;
    writeln!(writer, "total_source_only_rows,{}", summary.total_source_only_rows)?;
    writeln!(writer, "total_target_only_rows,{}", summary.total_target_only_rows)?;
    writeln!(writer, "total_modified_rows,{}", summary.total_modified_rows)?;
    writeln!(writer, "total_added_columns,{}", summary.total_added_columns)?;
    writeln!(writer, "total_removed_columns,{}", summary.total_removed_columns)?;
    Ok(())
}

fn write_batch_pair_results_csv(path: &Path, pair_results: &[BatchPairResult]) -> Result<()> {
    let mut writer = csv_writer(path)?;
    writeln!(
        writer,
        "name,source,target,status,source_rows,target_rows,source_only_rows,target_only_rows,modified_rows,added_columns,removed_columns,error"
    )?;
    for result in pair_results {
        writeln!(
            writer,
            "{},{},{},{},{},{},{},{},{},{},{},{}",
            csv_escape(&result.name),
            csv_escape(&result.source),
            csv_escape(&result.target),
            csv_escape(&result.status),
            result.source_rows,
            result.target_rows,
            result.source_only_rows,
            result.target_only_rows,
            result.modified_rows,
            result.added_columns,
            result.removed_columns,
            csv_escape(result.error.as_deref().unwrap_or(""))
        )?;
    }
    Ok(())
}

fn write_batch_top_columns_csv(path: &Path, top_columns: &[AggregatedChangedColumn]) -> Result<()> {
    let mut writer = csv_writer(path)?;
    writeln!(writer, "column,changed_rows")?;
    for entry in top_columns {
        writeln!(writer, "{},{}", csv_escape(&entry.column), entry.changed_rows)?;
    }
    Ok(())
}

fn csv_writer(path: &Path) -> Result<BufWriter<File>> {
    let file = File::create(path)?;
    Ok(BufWriter::new(file))
}

fn csv_escape(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn percentage(count: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        (count as f64 / total as f64) * 100.0
    }
}

fn format_opt_f64(value: Option<f64>) -> String {
    value
        .map(|v| format!("{v:.3}"))
        .unwrap_or_else(|| "-".to_string())
}

fn format_csv_opt_f64(value: Option<f64>) -> String {
    value.map(|v| format!("{v:.3}")).unwrap_or_default()
}

fn joined_or_dash(values: &[String]) -> String {
    if values.is_empty() {
        "-".to_string()
    } else {
        values.join(", ")
    }
}

fn is_numeric(dtype: &DataType) -> bool {
    matches!(
        dtype,
        DataType::Int8
            | DataType::Int16
            | DataType::Int32
            | DataType::Int64
            | DataType::UInt8
            | DataType::UInt16
            | DataType::UInt32
            | DataType::UInt64
            | DataType::Float32
            | DataType::Float64
    )
}

pub fn validate_export_args(output: Option<&str>, format: Option<&ExportFormat>, temp: bool) -> Result<()> {
    if temp && (output.is_some() || format.is_some()) {
        return Err(anyhow!("--temp cannot be used together with --output or --format"));
    }

    match (output, format) {
        (Some(_), Some(_)) | (None, None) => Ok(()),
        (Some(_), None) => Err(anyhow!("--format must be provided when --output is used")),
        (None, Some(_)) => Err(anyhow!("--output must be provided when --format is used")),
    }
}

/// High-level diff entry point for use by the Tauri GUI (and other embedders).
/// Returns the full diff result as a JSON value — no terminal rendering, no file export.
pub fn run_diff(
    path1: &str,
    path2: &str,
    keys: &[String],
    exclude_columns: Option<&str>,
    only_columns: Option<&str>,
    numeric_tolerance: Option<f64>,
) -> Result<serde_json::Value, DataDiffError> {
    let options = DiffComputationOptions {
        exclude_columns,
        only_columns,
        numeric_tolerance,
        include_column_stats: true,
    };
    let payload = compute_diff_export(path1, path2, keys, &options)?;
    serde_json::to_value(&payload).map_err(|e| DataDiffError::CLICommandError(e.to_string()))
}

/// Same as `run_diff` but accepts pre-loaded DataFrames (e.g. from database connectors).
/// `source_label` and `target_label` are used in error messages only.
pub fn run_diff_frames(
    df1: polars::prelude::DataFrame,
    df2: polars::prelude::DataFrame,
    source_label: &str,
    target_label: &str,
    keys: &[String],
    exclude_columns: Option<&str>,
    only_columns: Option<&str>,
    numeric_tolerance: Option<f64>,
) -> Result<serde_json::Value, DataDiffError> {
    let options = DiffComputationOptions {
        exclude_columns,
        only_columns,
        numeric_tolerance,
        include_column_stats: true,
    };
    let payload = diff_dataframes(df1, df2, source_label, target_label, keys, &options)?;
    serde_json::to_value(&payload).map_err(|e| DataDiffError::CLICommandError(e.to_string()))
}

fn prompt_for_export(path1: &str, path2: &str) -> Result<Option<(String, ExportFormat)>> {
    println!("\nSave these diff results? [y/N]");
    print!("> ");
    io::stdout().flush()?;

    let mut response = String::new();
    io::stdin().lock().read_line(&mut response)?;
    let response = response.trim().to_ascii_lowercase();

    if !matches!(response.as_str(), "y" | "yes") {
        return Ok(None);
    }

    let default_stem = default_export_stem(path1, path2);
    let default_path = format!("{default_stem}.json");

    println!("Output path [{default_path}]:");
    print!("> ");
    io::stdout().flush()?;

    let mut path_input = String::new();
    io::stdin().lock().read_line(&mut path_input)?;
    let path_input = path_input.trim();
    let output_path = if path_input.is_empty() {
        default_path
    } else {
        path_input.to_string()
    };

    let export_format = if let Some(format) = infer_export_format(&output_path) {
        format
    } else {
        println!("Export format [json/csv] (default json):");
        print!("> ");
        io::stdout().flush()?;

        let mut format_input = String::new();
        io::stdin().lock().read_line(&mut format_input)?;
        parse_export_format_input(format_input.trim()).unwrap_or(ExportFormat::Json)
    };

    Ok(Some((output_path, export_format)))
}

fn default_export_stem(path1: &str, path2: &str) -> String {
    let source_stem = Path::new(path1)
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("source");
    let target_stem = Path::new(path2)
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("target");

    format!("{source_stem}_vs_{target_stem}_diff")
}

fn infer_export_format(path: &str) -> Option<ExportFormat> {
    match Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("json") => Some(ExportFormat::Json),
        Some("csv") => Some(ExportFormat::Csv),
        _ => None,
    }
}

fn parse_export_format_input(input: &str) -> Option<ExportFormat> {
    match input.trim().to_ascii_lowercase().as_str() {
        "csv" => Some(ExportFormat::Csv),
        "json" | "" => Some(ExportFormat::Json),
        _ => None,
    }
}
