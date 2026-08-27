use crate::connectors;
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

#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum FailOn {
    Any,
    Breaking,
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
            DataDiffError::DataContentError(msg) => {
                write!(f, "Error while processing data: {}", msg)
            }
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

fn load_df(
    source: &str,
    query: Option<&str>,
    rt: &tokio::runtime::Runtime,
) -> Result<DataFrame, DataDiffError> {
    let config = connectors::parse_source_uri(source, query)
        .map_err(|e| DataDiffError::DataContentError(e.to_string()))?;
    rt.block_on(connectors::load_source(&config))
        .map_err(|e| DataDiffError::DataContentError(e.to_string()))
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
        if keys.contains(&col_name.to_string()) {
            return false;
        }
        if !self.only.is_empty() {
            return self.only.contains(col_name);
        }
        if !self.exclude.is_empty() {
            return !self.exclude.contains(col_name);
        }
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
    source_query: Option<String>,
    target_query: Option<String>,
    key: Option<String>,
    output_base: Option<String>,
    exclude_columns: Option<String>,
    only_columns: Option<String>,
    numeric_tolerance: Option<f64>,
    numeric_tolerance_percent: Option<f64>,
    diffs_only: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct BatchCsvManifestEntry {
    name: Option<String>,
    source: String,
    target: String,
    source_query: Option<String>,
    target_query: Option<String>,
    key: Option<String>,
    output_base: Option<String>,
    exclude_columns: Option<String>,
    only_columns: Option<String>,
    numeric_tolerance: Option<f64>,
    numeric_tolerance_percent: Option<f64>,
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
    numeric_tolerance: Option<Tolerance>,
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

/// Build a HashMap of composite keys to row indices.
///
/// Errors if any key value occurs more than once. A keyed diff pairs rows
/// one-to-one, so duplicate keys have no correct answer — silently keeping the
/// last occurrence would drop rows from the comparison without saying so.
fn build_composite_key_map(
    df: &DataFrame,
    keys: &[String],
    label: &str,
) -> Result<HashMap<String, usize>> {
    let mut map = HashMap::with_capacity(df.height());
    let mut duplicates: Vec<String> = Vec::new();
    let key_series = build_composite_key_column(df, keys)?;
    if let Ok(key_str) = key_series.str() {
        for (idx, opt_key) in key_str.iter().enumerate() {
            if let Some(key_val) = opt_key {
                if map.insert(key_val.to_string(), idx).is_some() {
                    duplicates.push(key_val.to_string());
                }
            }
        }
    }

    if !duplicates.is_empty() {
        duplicates.sort();
        duplicates.dedup();
        let sample: Vec<String> = duplicates.iter().take(5).cloned().collect();
        let remainder = duplicates.len() - sample.len();
        let suffix = if remainder > 0 {
            format!(" and {} more", remainder)
        } else {
            String::new()
        };
        return Err(anyhow!(
            "{} has {} duplicate value{} for key column{} [{}]: {}{}. \
             Rows sharing a key cannot be paired one-to-one — add another key column \
             to make the key unique, or de-duplicate the input.",
            label,
            duplicates.len(),
            if duplicates.len() == 1 { "" } else { "s" },
            if keys.len() == 1 { "" } else { "s" },
            keys.join(", "),
            sample.join(", "),
            suffix
        ));
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

/// How far two numeric values may differ before they count as changed.
///
/// Both variants are inclusive: a difference exactly equal to the threshold
/// still compares equal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tolerance {
    /// Equal when `|left - right| <= value`.
    Absolute(f64),
    /// Equal when `|left - right| / max(|left|, |right|) <= fraction`.
    ///
    /// Held as a fraction, so 5% is `Proportional(0.05)`.
    Proportional(f64),
}

impl Tolerance {
    /// Build a tolerance from the two mutually exclusive user-facing inputs.
    ///
    /// `percent` is given in percentage points (`5.0` meaning five percent) and
    /// is stored as a fraction. Returns `Ok(None)` when neither is supplied.
    pub fn resolve(
        absolute: Option<f64>,
        percent: Option<f64>,
    ) -> Result<Option<Self>, DataDiffError> {
        match (absolute, percent) {
            (Some(_), Some(_)) => Err(DataDiffError::CLICommandError(
                "Cannot use both an absolute and a proportional numeric tolerance".to_string(),
            )),
            (Some(value), None) => {
                Self::check_non_negative(value, "numeric tolerance")?;
                Ok(Some(Tolerance::Absolute(value)))
            }
            (None, Some(value)) => {
                Self::check_non_negative(value, "numeric tolerance percent")?;
                Ok(Some(Tolerance::Proportional(value / 100.0)))
            }
            (None, None) => Ok(None),
        }
    }

    fn check_non_negative(value: f64, label: &str) -> Result<(), DataDiffError> {
        if value.is_nan() || value < 0.0 {
            return Err(DataDiffError::CLICommandError(format!(
                "Invalid {}: {} — must be zero or greater",
                label, value
            )));
        }
        Ok(())
    }

    /// Whether two numbers are within this tolerance.
    fn matches(self, left: f64, right: f64) -> bool {
        let difference = (left - right).abs();
        match self {
            Tolerance::Absolute(value) => difference <= value,
            Tolerance::Proportional(fraction) => {
                // Scale by the larger magnitude so the comparison stays
                // symmetric. Both values being exactly zero is the only way the
                // scale is zero, and that is a match by definition.
                let scale = left.abs().max(right.abs());
                if scale == 0.0 {
                    return true;
                }
                difference / scale <= fraction
            }
        }
    }
}

/// Compare two values with optional numeric tolerance
fn values_equal(
    left: &polars::prelude::AnyValue,
    right: &polars::prelude::AnyValue,
    tolerance: Option<Tolerance>,
) -> bool {
    if let Some(tol) = tolerance {
        if let (Some(left_num), Some(right_num)) = (anyvalue_to_f64(left), anyvalue_to_f64(right)) {
            return tol.matches(left_num, right_num);
        }
    }

    left == right
}

pub fn data_diff(
    source: &str,
    target: &str,
    keys: &[String],
    source_query: Option<&str>,
    target_query: Option<&str>,
    output: Option<&str>,
    format: Option<ExportFormat>,
    temp: bool,
    exclude_columns: Option<&str>,
    only_columns: Option<&str>,
    numeric_tolerance: Option<Tolerance>,
    diffs_only: bool,
    json_output: bool,
) -> Result<()> {
    let options = DiffComputationOptions {
        exclude_columns,
        only_columns,
        numeric_tolerance,
        include_column_stats: !diffs_only || !temp || output.is_some() || format.is_some(),
    };

    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| anyhow!("Failed to create async runtime: {}", e))?;
    let df1 = load_df(source, source_query, &rt)?;
    let df2 = load_df(target, target_query, &rt)?;
    let export_payload = diff_dataframes(df1, df2, source, target, keys, &options)?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&export_payload)?);
        return Ok(());
    }

    render_diff_report(source, target, keys, &export_payload, diffs_only);

    if temp {
        return Ok(());
    }

    if let (Some(output_path), Some(export_format)) = (output, format) {
        let export_folder = create_export_folder()?;
        let export_base = export_path_in_folder(&export_folder, output_path);
        export_diff(
            export_base.to_str().unwrap(),
            export_format,
            &export_payload,
        )?;
        println!("\nExported results to: {}", export_folder.display());
    } else if let Some((prompt_path, prompt_format)) = prompt_for_export(source, target)? {
        let export_folder = create_export_folder()?;
        let export_base = export_path_in_folder(&export_folder, &prompt_path);
        export_diff(
            export_base.to_str().unwrap(),
            prompt_format,
            &export_payload,
        )?;
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
    numeric_tolerance: Option<Tolerance>,
    diffs_only: bool,
    fail_fast: bool,
) -> Result<()> {
    if keys.is_empty() {
        return Err(anyhow!("At least one key column must be specified"));
    }

    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| anyhow!("Failed to create async runtime: {}", e))?;
    let manifest_entries = read_batch_manifest(manifest_path, manifest_format)?;
    if manifest_entries.is_empty() {
        return Err(anyhow!(
            "Batch manifest does not contain any source/target pairs"
        ));
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
            numeric_tolerance: Tolerance::resolve(
                entry.numeric_tolerance,
                entry.numeric_tolerance_percent,
            )?
            .or(numeric_tolerance),
            include_column_stats: output.is_some() || format.is_some() || !pair_diffs_only,
        };

        let source_label = entry.source.clone();
        let target_label = entry.target.clone();
        let pair_result = (|| -> Result<DiffExport, DataDiffError> {
            let df1 = load_df(&source_label, entry.source_query.as_deref(), &rt)?;
            let df2 = load_df(&target_label, entry.target_query.as_deref(), &rt)?;
            diff_dataframes(
                df1,
                df2,
                &source_label,
                &target_label,
                &pair_keys,
                &pair_options,
            )
        })();

        match pair_result {
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
                        .unwrap_or_else(|| {
                            sanitize_file_component(&format!("{}_{}", output_base, pair_name))
                        });
                    let pair_base = folder.join("pairs").join(pair_output_base);
                    export_diff(
                        pair_base.to_str().unwrap(),
                        export_format.clone(),
                        &export_payload,
                    )?;
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
                    added_columns: export_payload
                        .column_summary
                        .column_presence
                        .added_in_target
                        .len(),
                    removed_columns: export_payload
                        .column_summary
                        .column_presence
                        .removed_from_source
                        .len(),
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

    if let (Some(output_base), Some(export_format), Some(folder)) =
        (output, format, export_folder.as_ref())
    {
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
            return Err(DataDiffError::MissingKeyColumn(format!(
                "Key column '{}' not found in source: {}",
                key, source_label
            )));
        }
        if !df2.get_column_names().contains(&key.as_str()) {
            return Err(DataDiffError::MissingKeyColumn(format!(
                "Key column '{}' not found in target: {}",
                key, target_label
            )));
        }
    }

    // Build row index maps using composite keys (optimized with HashMap capacity pre-allocation)
    let map1: HashMap<String, usize> = build_composite_key_map(&df1, keys, source_label)?;
    let map2: HashMap<String, usize> = build_composite_key_map(&df2, keys, target_label)?;

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

    let shared_keys_count = keys1.len() - source_only.len(); // Shared rows = total in source - source-only
    let row_summary = build_row_summary(
        df1.height(),
        df2.height(),
        target_only.len(),
        source_only.len(),
        modified.len(),
        shared_keys_count,
    );
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
    let change_summary =
        build_change_summary(&comparable_columns, &changed_column_counts, modified.len());

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
            percent_of_changed_rows: percentage(
                *changed_column_counts.get(column).unwrap_or(&0),
                changed_rows,
            ),
        })
        .collect()
}

fn render_diff_report(
    path1: &str,
    path2: &str,
    keys: &[String],
    payload: &DiffExport,
    diffs_only: bool,
) {
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

    print_key_table(
        "Rows only in target",
        Some(path2),
        keys,
        &payload.target_only,
    );
    print_key_table(
        "Rows only in source",
        Some(path1),
        keys,
        &payload.source_only,
    );
    print_key_table("Modified rows", None, keys, &payload.modified);
}

fn read_batch_manifest(
    path: &str,
    manifest_format: Option<ManifestFormat>,
) -> Result<Vec<BatchManifestEntry>> {
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
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_path(path)?;
    let mut entries = Vec::new();

    for row in reader.deserialize::<BatchCsvManifestEntry>() {
        let row = row?;
        entries.push(BatchManifestEntry {
            name: row.name,
            source: row.source,
            target: row.target,
            source_query: row.source_query,
            target_query: row.target_query,
            key: row.key,
            output_base: row.output_base,
            exclude_columns: row.exclude_columns,
            only_columns: row.only_columns,
            numeric_tolerance: row.numeric_tolerance,
            numeric_tolerance_percent: row.numeric_tolerance_percent,
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
    let succeeded_pairs = pair_results
        .iter()
        .filter(|result| result.status == "ok")
        .count();
    let failed_pairs = pair_results.len().saturating_sub(succeeded_pairs);

    let mut top_changed_columns: Vec<AggregatedChangedColumn> = aggregated_columns
        .into_iter()
        .filter(|(_, changed_rows)| *changed_rows > 0)
        .map(|(column, changed_rows)| AggregatedChangedColumn {
            column,
            changed_rows,
        })
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
        total_source_only_rows: pair_results
            .iter()
            .map(|result| result.source_only_rows)
            .sum(),
        total_target_only_rows: pair_results
            .iter()
            .map(|result| result.target_only_rows)
            .sum(),
        total_modified_rows: pair_results.iter().map(|result| result.modified_rows).sum(),
        total_added_columns: pair_results.iter().map(|result| result.added_columns).sum(),
        total_removed_columns: pair_results
            .iter()
            .map(|result| result.removed_columns)
            .sum(),
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
    table.add_row(row![
        "Total source-only rows",
        summary.total_source_only_rows
    ]);
    table.add_row(row![
        "Total target-only rows",
        summary.total_target_only_rows
    ]);
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
    table.add_row(row![
        "Total rows",
        row_summary.source_rows,
        row_summary.target_rows,
        "-"
    ]);
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
        "Column", "Type", "Nulls", "Unique", "Min", "Max", "Mean"
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

fn print_key_table(
    title: &str,
    dataset_label: Option<&str>,
    keys: &[String],
    key_values: &[String],
) {
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
        "biject_export"
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
    write_key_csv(
        &csv_output_path(base_path, "target_only"),
        &payload.key_columns,
        &payload.target_only,
    )?;
    write_key_csv(
        &csv_output_path(base_path, "source_only"),
        &payload.key_columns,
        &payload.source_only,
    )?;
    write_key_csv(
        &csv_output_path(base_path, "modified"),
        &payload.key_columns,
        &payload.modified,
    )?;
    write_row_summary_csv(
        &csv_output_path(base_path, "row_summary"),
        &payload.row_summary,
    )?;
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
    write_change_summary_csv(
        &csv_output_path(base_path, "change_summary"),
        &payload.change_summary,
    )?;

    Ok(())
}

fn export_batch_csv(output_path: &str, payload: &BatchExport) -> Result<()> {
    let base_path = Path::new(output_path);
    write_batch_summary_csv(
        &csv_output_path(base_path, "batch_summary"),
        &payload.summary,
    )?;
    write_batch_pair_results_csv(
        &csv_output_path(base_path, "batch_pairs"),
        &payload.pair_results,
    )?;
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
        .unwrap_or("biject_export");

    parent.join(format!("{stem}_{suffix}.csv"))
}

fn write_key_csv(path: &Path, key_columns: &[String], key_values: &[String]) -> Result<()> {
    let mut writer = csv_writer(path)?;
    // Write header with all key column names
    writeln!(
        writer,
        "{}",
        key_columns
            .iter()
            .map(|k| csv_escape(k))
            .collect::<Vec<_>>()
            .join(",")
    )?;
    // Write rows with composite key values (split by separator for display)
    for composite_key in key_values {
        let parts: Vec<&str> = composite_key.split(COMPOSITE_KEY_SEP).collect();
        writeln!(
            writer,
            "{}",
            parts
                .iter()
                .map(|p| csv_escape(p))
                .collect::<Vec<_>>()
                .join(",")
        )?;
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
    writeln!(
        writer,
        "dataset,column,data_type,null_count,unique_count,min,max,mean"
    )?;
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
    writeln!(
        writer,
        "total_source_only_rows,{}",
        summary.total_source_only_rows
    )?;
    writeln!(
        writer,
        "total_target_only_rows,{}",
        summary.total_target_only_rows
    )?;
    writeln!(
        writer,
        "total_modified_rows,{}",
        summary.total_modified_rows
    )?;
    writeln!(
        writer,
        "total_added_columns,{}",
        summary.total_added_columns
    )?;
    writeln!(
        writer,
        "total_removed_columns,{}",
        summary.total_removed_columns
    )?;
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
        writeln!(
            writer,
            "{},{}",
            csv_escape(&entry.column),
            entry.changed_rows
        )?;
    }
    Ok(())
}

fn csv_writer(path: &Path) -> Result<BufWriter<File>> {
    let file = File::create(path)?;
    Ok(BufWriter::new(file))
}

pub(crate) fn csv_escape(value: &str) -> String {
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

pub fn validate_export_args(
    output: Option<&str>,
    format: Option<&ExportFormat>,
    temp: bool,
) -> Result<()> {
    if temp && (output.is_some() || format.is_some()) {
        return Err(anyhow!(
            "--temp cannot be used together with --output or --format"
        ));
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
    numeric_tolerance: Option<Tolerance>,
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
    numeric_tolerance: Option<Tolerance>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use polars::prelude::AnyValue;

    // ---- values_equal: no tolerance ----

    #[test]
    fn without_tolerance_values_compare_exactly() {
        assert!(values_equal(&AnyValue::Int64(5), &AnyValue::Int64(5), None));
        assert!(!values_equal(
            &AnyValue::Int64(5),
            &AnyValue::Int64(6),
            None
        ));
        assert!(values_equal(
            &AnyValue::String("a"),
            &AnyValue::String("a"),
            None
        ));
        assert!(!values_equal(
            &AnyValue::String("a"),
            &AnyValue::String("b"),
            None
        ));
    }

    #[test]
    fn null_equals_null_and_differs_from_a_value() {
        assert!(values_equal(&AnyValue::Null, &AnyValue::Null, None));
        assert!(!values_equal(&AnyValue::Null, &AnyValue::Int64(0), None));
        // A null on either side is not numeric, so tolerance must not rescue it.
        assert!(!values_equal(
            &AnyValue::Null,
            &AnyValue::Int64(0),
            Some(Tolerance::Absolute(100.0))
        ));
    }

    // ---- values_equal: tolerance is an ABSOLUTE difference ----
    //
    // The comparison is |left - right| <= tolerance and has never been
    // proportional, despite the CLI help having once advertised "0-1 for
    // percentage". These tests pin the absolute semantics so the help text and
    // the implementation cannot drift apart again.

    #[test]
    fn tolerance_is_absolute_never_proportional() {
        // 2% apart. Under percentage semantics a tolerance of 0.05 would call
        // these equal; under absolute semantics it cannot.
        assert!(!values_equal(
            &AnyValue::Float64(50_000.0),
            &AnyValue::Float64(51_000.0),
            Some(Tolerance::Absolute(0.05))
        ));
        // The same pair is equal once the tolerance genuinely covers the gap.
        assert!(values_equal(
            &AnyValue::Float64(50_000.0),
            &AnyValue::Float64(51_000.0),
            Some(Tolerance::Absolute(1_000.0))
        ));
    }

    #[test]
    fn tolerance_boundary_is_inclusive() {
        // The comparison is <=, so a difference exactly equal to the tolerance
        // counts as equal.
        assert!(values_equal(
            &AnyValue::Float64(1.0),
            &AnyValue::Float64(1.5),
            Some(Tolerance::Absolute(0.5))
        ));
        assert!(!values_equal(
            &AnyValue::Float64(1.0),
            &AnyValue::Float64(1.6),
            Some(Tolerance::Absolute(0.5))
        ));
    }

    #[test]
    fn tolerance_is_symmetric_and_handles_negatives() {
        assert!(values_equal(
            &AnyValue::Int64(10),
            &AnyValue::Int64(8),
            Some(Tolerance::Absolute(2.0))
        ));
        assert!(values_equal(
            &AnyValue::Int64(8),
            &AnyValue::Int64(10),
            Some(Tolerance::Absolute(2.0))
        ));
        assert!(values_equal(
            &AnyValue::Int64(-5),
            &AnyValue::Int64(-7),
            Some(Tolerance::Absolute(2.0))
        ));
        assert!(!values_equal(
            &AnyValue::Int64(-5),
            &AnyValue::Int64(-8),
            Some(Tolerance::Absolute(2.0))
        ));
    }

    #[test]
    fn zero_tolerance_still_compares_numerically_across_types() {
        // A zero tolerance is Some(0.0), not None, so it takes the numeric path.
        // That makes an integer and a float of the same value compare equal,
        // where exact AnyValue equality would not.
        assert!(values_equal(
            &AnyValue::Int64(5),
            &AnyValue::Float64(5.0),
            Some(Tolerance::Absolute(0.0))
        ));
        assert!(!values_equal(
            &AnyValue::Int64(5),
            &AnyValue::Float64(5.0),
            None
        ));
    }

    #[test]
    fn tolerance_bridges_mixed_integer_and_float_widths() {
        assert!(values_equal(
            &AnyValue::Int32(100),
            &AnyValue::Float64(100.4),
            Some(Tolerance::Absolute(0.5))
        ));
        assert!(values_equal(
            &AnyValue::UInt8(7),
            &AnyValue::Int64(7),
            Some(Tolerance::Absolute(0.0))
        ));
    }

    #[test]
    fn tolerance_is_ignored_for_non_numeric_values() {
        // Strings are not convertible to f64, so the comparison falls through
        // to exact equality no matter how large the tolerance is.
        assert!(!values_equal(
            &AnyValue::String("100"),
            &AnyValue::String("101"),
            Some(Tolerance::Absolute(1_000.0))
        ));
        assert!(values_equal(
            &AnyValue::String("same"),
            &AnyValue::String("same"),
            Some(Tolerance::Absolute(1_000.0))
        ));
        assert!(!values_equal(
            &AnyValue::Boolean(true),
            &AnyValue::Boolean(false),
            Some(Tolerance::Absolute(1.0))
        ));
    }

    #[test]
    fn nan_is_never_equal_even_within_tolerance() {
        // NaN comparisons are false under IEEE 754, so a NaN on either side
        // always reports a difference. Worth pinning: it means a column of NaNs
        // reports every row as modified rather than silently matching.
        let nan = AnyValue::Float64(f64::NAN);
        assert!(!values_equal(&nan, &nan, Some(Tolerance::Absolute(1.0))));
        assert!(!values_equal(
            &nan,
            &AnyValue::Float64(1.0),
            Some(Tolerance::Absolute(1_000.0))
        ));
    }

    // ---- column filtering ----

    #[test]
    fn key_columns_are_never_compared() {
        let keys = vec!["id".to_string()];
        let filters = ColumnFilterSet::new(None, None);
        assert!(!filters.should_include("id", &keys));
        assert!(filters.should_include("name", &keys));
    }

    #[test]
    fn only_columns_takes_precedence_over_exclude() {
        let keys: Vec<String> = vec![];
        let filters = ColumnFilterSet::new(Some("name"), Some("name,email"));
        // `only` wins outright; the exclude list is not consulted.
        assert!(filters.should_include("name", &keys));
        assert!(filters.should_include("email", &keys));
        assert!(!filters.should_include("salary", &keys));
    }

    #[test]
    fn exclude_columns_drops_only_the_named_columns() {
        let keys: Vec<String> = vec![];
        let filters = ColumnFilterSet::new(Some("salary,notes"), None);
        assert!(!filters.should_include("salary", &keys));
        assert!(!filters.should_include("notes", &keys));
        assert!(filters.should_include("name", &keys));
    }

    #[test]
    fn column_lists_tolerate_whitespace_and_empty_input() {
        let parsed = parse_column_list(Some(" name , email "));
        assert!(parsed.contains("name"));
        assert!(parsed.contains("email"));
        assert_eq!(parsed.len(), 2);

        assert!(parse_column_list(Some("")).is_empty());
        assert!(parse_column_list(None).is_empty());
    }

    // ---- manifest key parsing ----

    #[test]
    fn manifest_keys_split_and_trim() {
        let parsed = parse_manifest_keys("user_id, date").unwrap();
        assert_eq!(parsed, vec!["user_id".to_string(), "date".to_string()]);
    }

    #[test]
    fn manifest_keys_reject_empty_and_separator_only_input() {
        assert!(parse_manifest_keys("").is_err());
        assert!(parse_manifest_keys("   ").is_err());
        assert!(parse_manifest_keys(",,,").is_err());
    }

    // ---- composite keys ----

    fn frame(ids: &[i64], dates: &[&str]) -> DataFrame {
        df!("id" => ids.to_vec(), "date" => dates.to_vec()).unwrap()
    }

    #[test]
    fn single_key_maps_each_row_to_its_index() {
        let df = frame(&[1, 2, 3], &["a", "b", "c"]);
        let map = build_composite_key_map(&df, &["id".to_string()], "source").unwrap();
        assert_eq!(map.len(), 3);
        assert_eq!(
            map.values().copied().collect::<HashSet<_>>(),
            HashSet::from([0, 1, 2])
        );
    }

    #[test]
    fn composite_key_combines_all_key_columns() {
        let df = frame(&[1, 1], &["2024-01-01", "2024-01-02"]);
        let keys = vec!["id".to_string(), "date".to_string()];
        let map = build_composite_key_map(&df, &keys, "source").unwrap();
        // A duplicated id is still two distinct rows once the date participates.
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn duplicate_composite_keys_are_rejected() {
        // A keyed diff pairs rows one-to-one, so duplicates have no correct
        // answer. Erroring is the only option that cannot silently drop rows.
        let df = frame(&[1, 1], &["2024-01-01", "2024-01-01"]);
        let keys = vec!["id".to_string(), "date".to_string()];
        let err = build_composite_key_map(&df, &keys, "source")
            .expect_err("duplicate keys must not be accepted");
        let message = err.to_string();

        assert!(
            message.contains("source"),
            "names the offending side: {message}"
        );
        assert!(
            message.contains("duplicate"),
            "says what is wrong: {message}"
        );
        assert!(
            message.contains("id, date"),
            "names the key columns: {message}"
        );
    }

    #[test]
    fn duplicate_key_error_distinguishes_source_from_target() {
        let df = frame(&[7, 7], &["a", "a"]);
        let keys = vec!["id".to_string(), "date".to_string()];
        let err = build_composite_key_map(&df, &keys, "target.csv").unwrap_err();
        assert!(err.to_string().contains("target.csv"));
    }

    #[test]
    fn unique_keys_are_still_accepted() {
        let df = frame(&[1, 2], &["a", "a"]);
        let keys = vec!["id".to_string(), "date".to_string()];
        assert_eq!(
            build_composite_key_map(&df, &keys, "source").unwrap().len(),
            2
        );
    }

    // ---- proportional tolerance ----

    #[test]
    fn proportional_tolerance_scales_with_magnitude() {
        // 1,000 apart on 51,000 is under 2%. This is the case absolute
        // tolerance cannot express without knowing the magnitude up front.
        let two_percent = Some(Tolerance::Proportional(0.02));
        assert!(values_equal(
            &AnyValue::Float64(50_000.0),
            &AnyValue::Float64(51_000.0),
            two_percent
        ));

        let one_percent = Some(Tolerance::Proportional(0.01));
        assert!(!values_equal(
            &AnyValue::Float64(50_000.0),
            &AnyValue::Float64(51_000.0),
            one_percent
        ));

        // The same proportion applied to small numbers stays proportional.
        assert!(values_equal(
            &AnyValue::Float64(5.0),
            &AnyValue::Float64(5.1),
            two_percent
        ));
    }

    #[test]
    fn proportional_tolerance_is_symmetric() {
        let tol = Some(Tolerance::Proportional(0.1));
        assert_eq!(
            values_equal(&AnyValue::Float64(100.0), &AnyValue::Float64(105.0), tol),
            values_equal(&AnyValue::Float64(105.0), &AnyValue::Float64(100.0), tol),
        );
    }

    #[test]
    fn proportional_tolerance_treats_zero_sensibly() {
        let tol = Some(Tolerance::Proportional(0.5));
        // Both exactly zero is a match; scaling by the larger magnitude would
        // otherwise divide by zero.
        assert!(values_equal(
            &AnyValue::Float64(0.0),
            &AnyValue::Float64(0.0),
            tol
        ));
        // Zero against anything else is a 100% difference.
        assert!(!values_equal(
            &AnyValue::Float64(0.0),
            &AnyValue::Float64(5.0),
            tol
        ));
        assert!(values_equal(
            &AnyValue::Float64(0.0),
            &AnyValue::Float64(5.0),
            Some(Tolerance::Proportional(1.0))
        ));
    }

    #[test]
    fn proportional_boundary_is_inclusive() {
        // Exactly 10% of the larger value.
        assert!(values_equal(
            &AnyValue::Float64(90.0),
            &AnyValue::Float64(100.0),
            Some(Tolerance::Proportional(0.1))
        ));
        assert!(!values_equal(
            &AnyValue::Float64(89.0),
            &AnyValue::Float64(100.0),
            Some(Tolerance::Proportional(0.1))
        ));
    }

    #[test]
    fn proportional_tolerance_rejects_nan() {
        let nan = AnyValue::Float64(f64::NAN);
        assert!(!values_equal(
            &nan,
            &nan,
            Some(Tolerance::Proportional(1.0))
        ));
        assert!(!values_equal(
            &nan,
            &AnyValue::Float64(1.0),
            Some(Tolerance::Proportional(1.0))
        ));
    }

    // ---- Tolerance::resolve ----

    #[test]
    fn resolve_returns_none_when_neither_is_given() {
        assert_eq!(Tolerance::resolve(None, None).unwrap(), None);
    }

    #[test]
    fn resolve_converts_percent_to_a_fraction() {
        match Tolerance::resolve(None, Some(5.0)).unwrap() {
            Some(Tolerance::Proportional(fraction)) => {
                assert!((fraction - 0.05).abs() < f64::EPSILON, "got {fraction}");
            }
            other => panic!("expected a proportional tolerance, got {other:?}"),
        }
    }

    #[test]
    fn resolve_passes_absolute_through_unchanged() {
        assert_eq!(
            Tolerance::resolve(Some(0.01), None).unwrap(),
            Some(Tolerance::Absolute(0.01))
        );
    }

    #[test]
    fn resolve_rejects_both_at_once() {
        assert!(Tolerance::resolve(Some(0.01), Some(5.0)).is_err());
    }

    #[test]
    fn resolve_rejects_negative_and_nan_input() {
        assert!(Tolerance::resolve(Some(-1.0), None).is_err());
        assert!(Tolerance::resolve(None, Some(-5.0)).is_err());
        assert!(Tolerance::resolve(Some(f64::NAN), None).is_err());
        assert!(Tolerance::resolve(None, Some(f64::NAN)).is_err());
    }

    // ---- diff_dataframes, end to end ----
    //
    // Fixtures are deliberately tiny and share a shape:
    //   source  id 1,2,3   target  id 1,2,4
    //   id 1 unchanged, id 2 amount 200 -> 250, id 3 source-only, id 4 target-only

    fn options<'a>(
        exclude: Option<&'a str>,
        only: Option<&'a str>,
        tolerance: Option<Tolerance>,
    ) -> DiffComputationOptions<'a> {
        DiffComputationOptions {
            exclude_columns: exclude,
            only_columns: only,
            numeric_tolerance: tolerance,
            include_column_stats: false,
        }
    }

    fn plain() -> DiffComputationOptions<'static> {
        options(None, None, None)
    }

    fn run(
        df1: DataFrame,
        df2: DataFrame,
        keys: &[&str],
        opts: &DiffComputationOptions,
    ) -> DiffExport {
        let keys: Vec<String> = keys.iter().map(|k| (*k).to_string()).collect();
        diff_dataframes(df1, df2, "source", "target", &keys, opts).unwrap()
    }

    fn source_frame() -> DataFrame {
        df!(
            "id" => [1i64, 2, 3],
            "name" => ["Alice", "Bob", "Carol"],
            "amount" => [100i64, 200, 300],
        )
        .unwrap()
    }

    fn target_frame() -> DataFrame {
        df!(
            "id" => [1i64, 2, 4],
            "name" => ["Alice", "Bob", "Dave"],
            "amount" => [100i64, 250, 400],
        )
        .unwrap()
    }

    #[test]
    fn identical_frames_report_no_differences() {
        let result = run(source_frame(), source_frame(), &["id"], &plain());
        assert!(result.source_only.is_empty());
        assert!(result.target_only.is_empty());
        assert!(result.modified.is_empty());
        assert_eq!(result.row_summary.modified_percent, 0.0);
    }

    #[test]
    fn unmatched_rows_are_split_by_side() {
        let result = run(source_frame(), target_frame(), &["id"], &plain());
        assert_eq!(result.source_only, vec!["3".to_string()]);
        assert_eq!(result.target_only, vec!["4".to_string()]);
    }

    #[test]
    fn changed_values_are_reported_as_modified() {
        let result = run(source_frame(), target_frame(), &["id"], &plain());
        // id 2 changed amount; id 1 is identical and must not appear.
        assert_eq!(result.modified, vec!["2".to_string()]);
    }

    #[test]
    fn key_columns_are_echoed_back_in_the_export() {
        let result = run(source_frame(), target_frame(), &["id"], &plain());
        assert_eq!(result.key_columns, vec!["id".to_string()]);
    }

    #[test]
    fn row_summary_counts_and_percentages() {
        let result = run(source_frame(), target_frame(), &["id"], &plain());
        let summary = &result.row_summary;

        assert_eq!(summary.source_rows, 3);
        assert_eq!(summary.target_rows, 3);
        assert_eq!(summary.source_only_rows, 1);
        assert_eq!(summary.target_only_rows, 1);
        assert_eq!(summary.modified_rows, 1);

        // Unmatched rows are a share of their own side's row count.
        assert!((summary.source_only_percent - 100.0 / 3.0).abs() < 1e-9);
        assert!((summary.target_only_percent - 100.0 / 3.0).abs() < 1e-9);
        // Modified is a share of the *shared* rows (2 here), not of either total.
        assert!((summary.modified_percent - 50.0).abs() < 1e-9);
    }

    #[test]
    fn change_summary_attributes_changes_to_the_right_column() {
        let result = run(source_frame(), target_frame(), &["id"], &plain());
        let by_column: HashMap<&str, &ChangedColumnSummary> = result
            .change_summary
            .iter()
            .map(|entry| (entry.column.as_str(), entry))
            .collect();

        assert_eq!(by_column["amount"].changed_rows, 1);
        assert!((by_column["amount"].percent_of_changed_rows - 100.0).abs() < 1e-9);
        // name is identical on every shared row, so it reports zero rather than
        // being omitted — consumers can rely on every comparable column appearing.
        assert_eq!(by_column["name"].changed_rows, 0);
        assert_eq!(by_column["name"].percent_of_changed_rows, 0.0);
        // The key column is never a comparable column.
        assert!(!by_column.contains_key("id"));
    }

    #[test]
    fn added_and_removed_columns_are_detected() {
        let source = df!("id" => [1i64], "gone" => ["x"]).unwrap();
        let target = df!("id" => [1i64], "fresh" => ["y"]).unwrap();
        let result = run(source, target, &["id"], &plain());

        assert_eq!(
            result.column_summary.column_presence.added_in_target,
            vec!["fresh".to_string()]
        );
        assert_eq!(
            result.column_summary.column_presence.removed_from_source,
            vec!["gone".to_string()]
        );
    }

    #[test]
    fn schema_only_changes_do_not_mark_rows_modified() {
        // A column present on one side only cannot be compared, so the row is
        // unchanged as far as data_diff is concerned. Schema drift is
        // schema_diff's job.
        let source = df!("id" => [1i64], "amount" => [10i64]).unwrap();
        let target = df!("id" => [1i64], "amount" => [10i64], "extra" => ["new"]).unwrap();
        let result = run(source, target, &["id"], &plain());

        assert!(result.modified.is_empty());
        assert_eq!(
            result.column_summary.column_presence.added_in_target,
            vec!["extra".to_string()]
        );
    }

    #[test]
    fn only_columns_restricts_what_is_compared() {
        // amount differs on id 2, name does not. Restricting to name hides it.
        let result = run(
            source_frame(),
            target_frame(),
            &["id"],
            &options(None, Some("name"), None),
        );
        assert!(result.modified.is_empty());
        assert_eq!(result.change_summary.len(), 1);
        assert_eq!(result.change_summary[0].column, "name");
    }

    #[test]
    fn exclude_columns_skips_the_named_column() {
        let result = run(
            source_frame(),
            target_frame(),
            &["id"],
            &options(Some("amount"), None, None),
        );
        assert!(result.modified.is_empty());
        assert!(result
            .change_summary
            .iter()
            .all(|entry| entry.column != "amount"));
    }

    #[test]
    fn exclude_and_only_together_is_rejected() {
        let keys = vec!["id".to_string()];
        let opts = options(Some("amount"), Some("name"), None);
        let err = diff_dataframes(
            source_frame(),
            target_frame(),
            "source",
            "target",
            &keys,
            &opts,
        )
        .unwrap_err();
        assert!(err.to_string().contains("Cannot use both"));
    }

    #[test]
    fn tolerance_suppresses_small_changes_in_a_real_diff() {
        // 200 -> 250 is a 20% change against the larger value.
        let within = run(
            source_frame(),
            target_frame(),
            &["id"],
            &options(None, None, Some(Tolerance::Proportional(0.25))),
        );
        assert!(within.modified.is_empty());

        let outside = run(
            source_frame(),
            target_frame(),
            &["id"],
            &options(None, None, Some(Tolerance::Proportional(0.1))),
        );
        assert_eq!(outside.modified, vec!["2".to_string()]);
    }

    #[test]
    fn missing_key_column_is_rejected_on_either_side() {
        let keys = vec!["nope".to_string()];
        let err = diff_dataframes(
            source_frame(),
            target_frame(),
            "source.csv",
            "target.csv",
            &keys,
            &plain(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("source.csv"), "{err}");

        let keys = vec!["name".to_string()];
        let target_missing = df!("name_other" => ["Alice"]).unwrap();
        let err = diff_dataframes(
            source_frame(),
            target_missing,
            "source.csv",
            "target.csv",
            &keys,
            &plain(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("target.csv"), "{err}");
    }

    #[test]
    fn composite_keys_pair_rows_across_frames() {
        let source = df!(
            "id" => [1i64, 1],
            "day" => ["mon", "tue"],
            "amount" => [10i64, 20],
        )
        .unwrap();
        let target = df!(
            "id" => [1i64, 1],
            "day" => ["mon", "tue"],
            "amount" => [10i64, 99],
        )
        .unwrap();

        let result = run(source, target, &["id", "day"], &plain());
        assert!(result.source_only.is_empty());
        assert!(result.target_only.is_empty());
        assert_eq!(result.modified.len(), 1, "only the tue row changed");
        assert!(result.modified[0].contains("tue"));
    }

    #[test]
    fn key_lists_are_sorted_for_deterministic_output() {
        let source = df!("id" => [5i64, 1, 3]).unwrap();
        let target = df!("id" => [9i64, 7]).unwrap();
        let result = run(source, target, &["id"], &plain());

        let mut expected_source = result.source_only.clone();
        expected_source.sort();
        assert_eq!(result.source_only, expected_source);

        let mut expected_target = result.target_only.clone();
        expected_target.sort();
        assert_eq!(result.target_only, expected_target);
    }

    // ---- DiffExport serialisation ----

    #[test]
    fn diff_export_json_keeps_its_shape() {
        // Guards the exported field names and nesting. Anything consuming the
        // --json output or a JSON export depends on this staying stable.
        let result = run(source_frame(), target_frame(), &["id"], &plain());
        let value = serde_json::to_value(&result).unwrap();
        let object = value.as_object().unwrap();

        let mut top: Vec<&str> = object.keys().map(|k| k.as_str()).collect();
        top.sort();
        assert_eq!(
            top,
            vec![
                "change_summary",
                "column_summary",
                "key_columns",
                "modified",
                "row_summary",
                "source_only",
                "target_only",
            ]
        );

        let mut row: Vec<&str> = object["row_summary"]
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.as_str())
            .collect();
        row.sort();
        assert_eq!(
            row,
            vec![
                "modified_percent",
                "modified_rows",
                "source_only_percent",
                "source_only_rows",
                "source_rows",
                "target_only_percent",
                "target_only_rows",
                "target_rows",
            ]
        );

        let mut column: Vec<&str> = object["column_summary"]
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.as_str())
            .collect();
        column.sort();
        assert_eq!(column, vec!["column_presence", "source", "target"]);

        // Values, not just shape.
        assert_eq!(object["key_columns"], serde_json::json!(["id"]));
        assert_eq!(object["source_only"], serde_json::json!(["3"]));
        assert_eq!(object["target_only"], serde_json::json!(["4"]));
        assert_eq!(object["modified"], serde_json::json!(["2"]));
        assert_eq!(object["row_summary"]["source_rows"], 3);
        assert_eq!(object["row_summary"]["modified_rows"], 1);

        let change: Vec<&str> = result
            .change_summary
            .iter()
            .map(|entry| entry.column.as_str())
            .collect();
        assert_eq!(change, vec!["amount", "name"], "sorted comparable columns");
    }

    // ---- CSV export ----

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("biject_test_{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn csv_export_writes_one_file_per_section() {
        let dir = scratch("csv_sections");
        let result = run(source_frame(), target_frame(), &["id"], &plain());
        export_csv(dir.join("out").to_str().unwrap(), &result).unwrap();

        for suffix in [
            "source_only",
            "target_only",
            "modified",
            "row_summary",
            "column_summary_source",
            "column_summary_target",
            "column_presence",
            "change_summary",
        ] {
            let path = dir.join(format!("out_{suffix}.csv"));
            assert!(path.exists(), "missing {}", path.display());
        }

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn csv_key_files_split_composite_keys_into_columns() {
        let dir = scratch("csv_composite");
        let source = df!(
            "id" => [1i64, 2],
            "day" => ["mon", "tue"],
            "amount" => [10i64, 20],
        )
        .unwrap();
        let target = df!(
            "id" => [1i64],
            "day" => ["mon"],
            "amount" => [10i64],
        )
        .unwrap();

        let result = run(source, target, &["id", "day"], &plain());
        export_csv(dir.join("out").to_str().unwrap(), &result).unwrap();

        let body = fs::read_to_string(dir.join("out_source_only.csv")).unwrap();
        let mut lines = body.lines();
        assert_eq!(lines.next().unwrap(), "id,day", "header names both keys");
        // The composite key is split back into one column per key, not emitted
        // as a single "2::tue" cell.
        let row = lines.next().unwrap();
        assert!(row.starts_with("2,"), "id first: {row}");
        assert!(row.contains("tue"), "day second: {row}");

        fs::remove_dir_all(&dir).ok();
    }

    // ---- column statistics ----

    #[test]
    fn column_stats_cover_every_column_in_order() {
        let df = df!("id" => [1i64, 2], "label" => ["a", "b"]).unwrap();
        let stats = build_column_stats(&df).unwrap();
        let names: Vec<&str> = stats.iter().map(|s| s.column.as_str()).collect();
        assert_eq!(names, vec!["id", "label"]);
    }

    #[test]
    fn numeric_columns_report_min_max_and_mean() {
        let df = df!("amount" => [10i64, 20, 60]).unwrap();
        let stats = build_column_stats(&df).unwrap();
        let amount = &stats[0];

        assert_eq!(amount.min, Some(10.0));
        assert_eq!(amount.max, Some(60.0));
        assert_eq!(amount.mean, Some(30.0));
    }

    #[test]
    fn non_numeric_columns_leave_min_max_and_mean_unset() {
        // The exporter emits null and the CLI prints "-" for these, so None is
        // load-bearing rather than incidental.
        let df = df!("label" => ["a", "b"]).unwrap();
        let stats = build_column_stats(&df).unwrap();

        assert_eq!(stats[0].min, None);
        assert_eq!(stats[0].max, None);
        assert_eq!(stats[0].mean, None);
    }

    #[test]
    fn column_stats_count_nulls_and_distinct_values() {
        let df = df!(
            "amount" => [Some(1i64), None, Some(1), Some(4)],
            "label" => [Some("x"), Some("x"), Some("y"), None],
        )
        .unwrap();
        let stats = build_column_stats(&df).unwrap();
        let by_name: HashMap<&str, &ColumnStats> =
            stats.iter().map(|s| (s.column.as_str(), s)).collect();

        assert_eq!(by_name["amount"].null_count, 1);
        assert_eq!(by_name["label"].null_count, 1);
        // n_unique counts null as a distinct value: {1, 4, null} and {x, y, null}.
        assert_eq!(by_name["amount"].unique_count, 3);
        assert_eq!(by_name["label"].unique_count, 3);
    }

    #[test]
    fn null_aware_statistics_ignore_missing_values() {
        // min/max/mean skip nulls rather than treating them as zero.
        let df = df!("amount" => [Some(10i64), None, Some(30)]).unwrap();
        let stats = build_column_stats(&df).unwrap();

        assert_eq!(stats[0].min, Some(10.0));
        assert_eq!(stats[0].max, Some(30.0));
        assert_eq!(stats[0].mean, Some(20.0));
    }

    #[test]
    fn an_all_null_numeric_column_has_no_statistics() {
        let df = df!("amount" => [None::<i64>, None]).unwrap();
        let stats = build_column_stats(&df).unwrap();

        assert_eq!(stats[0].null_count, 2);
        assert_eq!(stats[0].min, None);
        assert_eq!(stats[0].max, None);
        assert_eq!(stats[0].mean, None);
    }

    #[test]
    fn column_stats_record_the_declared_data_type() {
        let df = df!("amount" => [1i64], "label" => ["a"]).unwrap();
        let stats = build_column_stats(&df).unwrap();
        let by_name: HashMap<&str, &ColumnStats> =
            stats.iter().map(|s| (s.column.as_str(), s)).collect();

        assert_eq!(by_name["amount"].data_type, "Int64");
        assert_eq!(by_name["label"].data_type, "String");
    }

    #[test]
    fn column_stats_are_only_computed_when_requested() {
        // diffs_only runs skip stats entirely; the export must still be valid.
        let result = run(source_frame(), target_frame(), &["id"], &plain());
        assert!(result.column_summary.source.is_empty());
        assert!(result.column_summary.target.is_empty());

        let with_stats = DiffComputationOptions {
            exclude_columns: None,
            only_columns: None,
            numeric_tolerance: None,
            include_column_stats: true,
        };
        let result = run(source_frame(), target_frame(), &["id"], &with_stats);
        assert_eq!(result.column_summary.source.len(), 3);
        assert_eq!(result.column_summary.target.len(), 3);
    }

    // ---- batch manifests ----

    fn write(dir: &Path, name: &str, body: &str) -> String {
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        path.to_str().unwrap().to_string()
    }

    #[test]
    fn json_manifest_round_trips_every_field() {
        let dir = scratch("manifest_json");
        let path = write(
            &dir,
            "pairs.json",
            r#"[
              {
                "name": "first",
                "source": "a.csv",
                "target": "b.csv",
                "key": "id,day",
                "exclude_columns": "notes",
                "numeric_tolerance": 0.5,
                "diffs_only": true
              }
            ]"#,
        );

        let entries = read_batch_manifest(&path, None).unwrap();
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.name.as_deref(), Some("first"));
        assert_eq!(entry.source, "a.csv");
        assert_eq!(entry.target, "b.csv");
        assert_eq!(entry.key.as_deref(), Some("id,day"));
        assert_eq!(entry.exclude_columns.as_deref(), Some("notes"));
        assert_eq!(entry.numeric_tolerance, Some(0.5));
        assert_eq!(entry.numeric_tolerance_percent, None);
        assert_eq!(entry.diffs_only, Some(true));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn json_manifest_accepts_a_percentage_tolerance() {
        let dir = scratch("manifest_percent");
        let path = write(
            &dir,
            "pairs.json",
            r#"[{"source":"a.csv","target":"b.csv","numeric_tolerance_percent":5}]"#,
        );

        let entries = read_batch_manifest(&path, None).unwrap();
        assert_eq!(entries[0].numeric_tolerance_percent, Some(5.0));
        let resolved = Tolerance::resolve(
            entries[0].numeric_tolerance,
            entries[0].numeric_tolerance_percent,
        )
        .unwrap();
        assert_eq!(resolved, Some(Tolerance::Proportional(0.05)));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_manifest_entry_needs_only_source_and_target() {
        let dir = scratch("manifest_minimal");
        let path = write(
            &dir,
            "pairs.json",
            r#"[{"source":"a.csv","target":"b.csv"}]"#,
        );

        let entries = read_batch_manifest(&path, None).unwrap();
        assert_eq!(entries[0].source, "a.csv");
        assert!(entries[0].key.is_none(), "falls back to the global --key");
        assert!(entries[0].diffs_only.is_none());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn json_manifest_tolerates_a_byte_order_mark_and_whitespace() {
        // Manifests are hand-authored and frequently saved by editors that add
        // a BOM; the parser strips it rather than failing on byte one.
        let dir = scratch("manifest_bom");
        let path = write(
            &dir,
            "pairs.json",
            "\u{feff}\n  [{\"source\":\"a.csv\",\"target\":\"b.csv\"}]  \n",
        );

        let entries = read_batch_manifest(&path, None).unwrap();
        assert_eq!(entries.len(), 1);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_manifest_missing_a_required_field_is_rejected() {
        let dir = scratch("manifest_invalid");
        let path = write(&dir, "pairs.json", r#"[{"source":"a.csv"}]"#);
        assert!(
            read_batch_manifest(&path, None).is_err(),
            "target is required"
        );

        let path = write(&dir, "broken.json", "{not json");
        assert!(read_batch_manifest(&path, None).is_err());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_manifest_file_is_an_error_not_an_empty_batch() {
        let dir = scratch("manifest_absent");
        let path = dir.join("nope.json").to_str().unwrap().to_string();
        assert!(read_batch_manifest(&path, None).is_err());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn csv_manifests_are_parsed_by_extension() {
        let dir = scratch("manifest_csv");
        let path = write(
            &dir,
            "pairs.csv",
            "name,source,target,key,exclude_columns,numeric_tolerance,numeric_tolerance_percent,diffs_only\n\
             first,a.csv,b.csv,id,notes,0.5,,true\n\
             second,c.csv,d.csv,\"id,day\",,,5,false\n",
        );

        let entries = read_batch_manifest(&path, None).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name.as_deref(), Some("first"));
        assert_eq!(entries[0].numeric_tolerance, Some(0.5));
        assert_eq!(entries[0].diffs_only, Some(true));
        // A quoted comma survives as a single multi-column key.
        assert_eq!(entries[1].key.as_deref(), Some("id,day"));
        assert_eq!(entries[1].numeric_tolerance_percent, Some(5.0));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn manifest_format_can_be_forced_against_the_extension() {
        // A CSV manifest saved with a .txt extension would otherwise be parsed
        // as JSON and fail.
        let dir = scratch("manifest_forced");
        let path = write(&dir, "pairs.txt", "name,source,target\nfirst,a.csv,b.csv\n");

        assert!(
            read_batch_manifest(&path, None).is_err(),
            "inferred as JSON"
        );
        let entries = read_batch_manifest(&path, Some(ManifestFormat::Csv)).unwrap();
        assert_eq!(entries[0].source, "a.csv");

        fs::remove_dir_all(&dir).ok();
    }

    // ---- pair naming and path safety ----

    #[test]
    fn a_pair_without_a_name_is_named_after_its_files() {
        let entry = BatchManifestEntry {
            name: None,
            source: "data/customers_v1.csv".to_string(),
            target: "data/customers_v2.csv".to_string(),
            source_query: None,
            target_query: None,
            key: None,
            output_base: None,
            exclude_columns: None,
            only_columns: None,
            numeric_tolerance: None,
            numeric_tolerance_percent: None,
            diffs_only: None,
        };
        assert_eq!(batch_pair_name(&entry), "customers_v1_vs_customers_v2");
    }

    #[test]
    fn a_blank_pair_name_falls_back_rather_than_producing_an_empty_label() {
        let entry = BatchManifestEntry {
            name: Some("   ".to_string()),
            source: "a.csv".to_string(),
            target: "b.csv".to_string(),
            source_query: None,
            target_query: None,
            key: None,
            output_base: None,
            exclude_columns: None,
            only_columns: None,
            numeric_tolerance: None,
            numeric_tolerance_percent: None,
            diffs_only: None,
        };
        assert_eq!(batch_pair_name(&entry), "a_vs_b");
    }

    #[test]
    fn pair_names_are_sanitised_before_becoming_filenames() {
        // Pair names come from user-authored manifests and reach the filesystem
        // as export filenames, so separators and traversal sequences must not
        // survive. Leading and trailing underscores are trimmed, which is why
        // a traversal prefix collapses away entirely rather than becoming "___".
        assert_eq!(sanitize_file_component("a/b"), "a_b");
        assert_eq!(sanitize_file_component("../etc/passwd"), "etc_passwd");
        assert_eq!(
            sanitize_file_component("..\\windows\\system32"),
            "windows_system32"
        );
        assert_eq!(sanitize_file_component("with space"), "with_space");
        assert_eq!(sanitize_file_component("keep-_09"), "keep-_09");
    }

    #[test]
    fn sanitised_names_never_contain_path_syntax() {
        for hostile in [
            "../../etc/shadow",
            "C:\\Windows\\Temp",
            "name.with.dots",
            "trailing/",
            "*?<>|",
        ] {
            let safe = sanitize_file_component(hostile);
            assert!(
                !safe.contains(['/', '\\', '.', ':', '*', '?', '<', '>', '|']),
                "{hostile:?} sanitised to {safe:?}"
            );
        }
    }

    #[test]
    fn csv_row_summary_is_a_metric_value_table() {
        let dir = scratch("csv_row_summary");
        let result = run(source_frame(), target_frame(), &["id"], &plain());
        export_csv(dir.join("out").to_str().unwrap(), &result).unwrap();

        let body = fs::read_to_string(dir.join("out_row_summary.csv")).unwrap();
        assert!(body.starts_with("metric,value\n"));
        assert!(body.contains("source_rows,3"));
        assert!(body.contains("target_rows,3"));
        assert!(body.contains("modified_rows,1"));
        assert!(body.contains("modified_percent,50.000"), "3dp: {body}");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_key_list_is_rejected() {
        let df = frame(&[1], &["a"]);
        assert!(build_composite_key_map(&df, &[], "source").is_err());
    }
}
