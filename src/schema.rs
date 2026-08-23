use anyhow::{anyhow, Result};
use crate::catalog::{self, CatalogAvailability, MetadataChange};
use crate::connectors;
use polars::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum TypeChangeImpact {
    SafePromotion,
    RiskyConversion,
    Breaking,
}

#[derive(Clone, Debug)]
pub enum SchemaDiffError {
    MissingColumnType(String),
    PolicyViolation(String),
    InvalidPolicyFile(String),
    DataLoadError(String),
}

impl std::fmt::Display for SchemaDiffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchemaDiffError::MissingColumnType(col) => {
                write!(f, "Missing column type for: {}", col)
            }
            SchemaDiffError::PolicyViolation(msg) => {
                write!(f, "Schema policy violation: {}", msg)
            }
            SchemaDiffError::InvalidPolicyFile(msg) => {
                write!(f, "Invalid schema policy file: {}", msg)
            }
            SchemaDiffError::DataLoadError(msg) => {
                write!(f, "Data load error: {}", msg)
            }
        }
    }
}

impl std::error::Error for SchemaDiffError {}

impl From<serde_json::Error> for SchemaDiffError {
    fn from(err: serde_json::Error) -> Self {
        SchemaDiffError::InvalidPolicyFile(err.to_string())
    }
}

impl From<PolarsError> for SchemaDiffError {
    fn from(err: PolarsError) -> Self {
        SchemaDiffError::MissingColumnType(err.to_string())
    }
}

impl From<anyhow::Error> for SchemaDiffError {
    fn from(err: anyhow::Error) -> Self {
        SchemaDiffError::MissingColumnType(err.to_string())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TypeChange {
    pub column: String,
    pub source_type: String,
    pub target_type: String,
    pub impact: TypeChangeImpact,
}

#[derive(Debug, Clone, Serialize)]
pub struct RenameSuggestion {
    pub source_column: String,
    pub target_column: String,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompatibilitySummary {
    pub backward_compatible: bool,
    pub forward_compatible: bool,
    pub breaking_reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct SchemaDiffResult {
    pub source_path: String,
    pub target_path: String,
    /// Every source column mapped to its type, so consumers that need to act on
    /// `added` or `removed` can look up the type rather than reloading the data.
    pub source_schema: BTreeMap<String, String>,
    /// Every target column mapped to its type.
    pub target_schema: BTreeMap<String, String>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub type_changes: Vec<TypeChange>,
    pub rename_suggestions: Vec<RenameSuggestion>,
    pub compatibility: CompatibilitySummary,
    /// Schema detail only the database's own catalog can supply: declared
    /// types, nullability, defaults. Carries why it is missing when it is.
    pub metadata: MetadataReport,
    pub policy_violations: Vec<String>,
    pub policy_passed: Option<bool>,
}

/// Catalog-derived findings, and the availability that produced them.
///
/// `changes` being empty is meaningless without `source` and `target`: it means
/// "nothing differed" only when both are available, and "nothing was examined"
/// otherwise.
#[derive(Debug, Clone, Serialize)]
pub struct MetadataReport {
    pub source: CatalogAvailability,
    pub target: CatalogAvailability,
    pub changes: Vec<MetadataChange>,
}

impl MetadataReport {
    /// Both sides were read, so an empty `changes` genuinely means no change.
    pub fn is_complete(&self) -> bool {
        self.source.is_available() && self.target.is_available()
    }

    /// Reasons metadata could not be compared, one per side that is missing.
    pub fn gaps(&self) -> Vec<(&'static str, String)> {
        let mut gaps = Vec::new();
        if let Some(reason) = self.source.explain() {
            gaps.push(("source", reason));
        }
        if let Some(reason) = self.target.explain() {
            gaps.push(("target", reason));
        }
        gaps
    }
}

#[derive(Debug, Deserialize, Default)]
struct SchemaPolicy {
    required_columns_source: Option<Vec<String>>,
    required_columns_target: Option<Vec<String>>,
    forbidden_removals: Option<Vec<String>>,
    max_new_columns: Option<usize>,
    allowed_type_changes: Option<Vec<AllowedTypeChange>>,
    fail_on_breaking: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct AllowedTypeChange {
    from: String,
    to: String,
}

/// Returns structured schema diff data from pre-loaded DataFrames. Used by the GUI when sources are SQL Server or other connectors.
pub fn run_schema_diff_frames(df1: DataFrame, df2: DataFrame, source_label: &str, target_label: &str) -> Result<SchemaDiffResult, SchemaDiffError> {
    run_schema_diff_inner(&df1, &df2, source_label, target_label, None, None)
}

/// Returns structured schema diff data — no terminal output. Used by the GUI and `--json` mode.
pub fn run_schema_diff(path1: &str, path2: &str, policy_path: Option<&str>) -> Result<SchemaDiffResult, SchemaDiffError> {
    let df1 = CsvReader::from_path(path1)?
        .infer_schema(Some(100))
        .has_header(true)
        .finish()?;

    let df2 = CsvReader::from_path(path2)?
        .infer_schema(Some(100))
        .has_header(true)
        .finish()?;

    run_schema_diff_inner(&df1, &df2, path1, path2, policy_path, None)
}

fn run_schema_diff_inner(
    df1: &DataFrame,
    df2: &DataFrame,
    source_label: &str,
    target_label: &str,
    policy_path: Option<&str>,
    catalogs: Option<(CatalogAvailability, CatalogAvailability)>,
) -> Result<SchemaDiffResult, SchemaDiffError> {
    let source_schema = schema_map(df1)?;
    let target_schema = schema_map(df2)?;

    let source_cols: BTreeSet<String> = source_schema.keys().cloned().collect();
    let target_cols: BTreeSet<String> = target_schema.keys().cloned().collect();

    let added: Vec<String> = target_cols.difference(&source_cols).cloned().collect();
    let removed: Vec<String> = source_cols.difference(&target_cols).cloned().collect();

    let mut type_changes = Vec::new();
    for col in source_cols.intersection(&target_cols) {
        let source_ty = source_schema.get(col).ok_or_else(|| anyhow!("Missing source type for column: {col}"))?;
        let target_ty = target_schema.get(col).ok_or_else(|| anyhow!("Missing target type for column: {col}"))?;
        if source_ty != target_ty {
            type_changes.push(TypeChange {
                column: col.to_string(),
                source_type: source_ty.clone(),
                target_type: target_ty.clone(),
                impact: classify_type_change(source_ty, target_ty),
            });
        }
    }

    let rename_suggestions = detect_rename_suggestions(&removed, &added, &source_schema, &target_schema);

    // Absent catalogs are NotRequested rather than any other reason: the caller
    // did not ask, which is different from asking and being unable.
    let (source_catalog, target_catalog) = catalogs.unwrap_or((
        CatalogAvailability::NotRequested,
        CatalogAvailability::NotRequested,
    ));
    let metadata_changes = match (source_catalog.catalog(), target_catalog.catalog()) {
        (Some(source), Some(target)) => catalog::compare(source, target),
        _ => Vec::new(),
    };

    // Computed after the catalog so its findings reach the verdict.
    let compatibility =
        summarize_compatibility(&added, &removed, &type_changes, &metadata_changes);

    let metadata = MetadataReport {
        source: source_catalog,
        target: target_catalog,
        changes: metadata_changes,
    };

    let (policy_violations, policy_passed) = if let Some(path) = policy_path {
        let policy = load_policy(path)?;
        let violations = evaluate_policy(&policy, &source_cols, &target_cols, &added, &removed, &type_changes, &compatibility);
        let passed = violations.is_empty();
        if !violations.is_empty() && policy.fail_on_breaking.unwrap_or(true) {
            return Err(SchemaDiffError::PolicyViolation("Schema policy violations detected".to_string()));
        }
        (violations, Some(passed))
    } else {
        (Vec::new(), None)
    };

    Ok(SchemaDiffResult {
        source_path: source_label.to_string(),
        target_path: target_label.to_string(),
        source_schema: source_schema.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        target_schema: target_schema.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        added,
        removed,
        type_changes,
        rename_suggestions,
        compatibility,
        metadata,
        policy_violations,
        policy_passed,
    })
}

pub fn schema_diff(
    source: &str,
    target: &str,
    source_query: Option<&str>,
    target_query: Option<&str>,
    policy_path: Option<&str>,
    output: Option<&str>,
    format: Option<crate::data::ExportFormat>,
) -> Result<(), SchemaDiffError> {
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| SchemaDiffError::DataLoadError(format!("Failed to create async runtime: {}", e)))?;

    let source_config = connectors::parse_source_uri(source, source_query)
        .map_err(|e| SchemaDiffError::DataLoadError(e.to_string()))?;
    let target_config = connectors::parse_source_uri(target, target_query)
        .map_err(|e| SchemaDiffError::DataLoadError(e.to_string()))?;

    let df1 = rt.block_on(connectors::load_source(&source_config))
        .map_err(|e| SchemaDiffError::DataLoadError(e.to_string()))?;
    let df2 = rt.block_on(connectors::load_source(&target_config))
        .map_err(|e| SchemaDiffError::DataLoadError(e.to_string()))?;

    // Reading the catalog never fails the run: every reason it might be
    // unavailable is reported to the user instead.
    let catalogs = (
        rt.block_on(connectors::read_catalog(&source_config)),
        rt.block_on(connectors::read_catalog(&target_config)),
    );

    // Built through the same path as the library API rather than reimplemented
    // here. The two used to duplicate the whole comparison, which meant CLI
    // output and SchemaDiffResult could drift, and left the command with no
    // result object to export.
    let result = run_schema_diff_inner(&df1, &df2, source, target, policy_path, Some(catalogs))?;

    render_schema_report(&result, policy_path);

    if let (Some(path), Some(format)) = (output, format) {
        export_schema(path, format, &result)?;
        println!("
Exported schema comparison to: {path}");
    }

    Ok(())
}

/// Write a schema comparison to a file.
///
/// Unlike `data`, which writes several files into a generated folder, this is
/// a single document and goes exactly where it is told.
fn export_schema(
    path: &str,
    format: crate::data::ExportFormat,
    result: &SchemaDiffResult,
) -> Result<(), SchemaDiffError> {
    use std::io::Write;

    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| SchemaDiffError::DataLoadError(e.to_string()))?;
        }
    }

    match format {
        crate::data::ExportFormat::Json => {
            let json = serde_json::to_string_pretty(result)?;
            std::fs::write(path, json)
                .map_err(|e| SchemaDiffError::DataLoadError(e.to_string()))?;
        }
        crate::data::ExportFormat::Csv => {
            let file = std::fs::File::create(path)
                .map_err(|e| SchemaDiffError::DataLoadError(e.to_string()))?;
            let mut writer = std::io::BufWriter::new(file);
            for line in schema_csv_rows(result) {
                writeln!(writer, "{line}")
                    .map_err(|e| SchemaDiffError::DataLoadError(e.to_string()))?;
            }
            writer
                .flush()
                .map_err(|e| SchemaDiffError::DataLoadError(e.to_string()))?;
        }
    }

    Ok(())
}

/// Flatten a comparison into one row per finding.
///
/// Metadata availability is emitted as rows of its own. Without them a CSV
/// showing no metadata changes would be indistinguishable from one where
/// metadata was never examined — the same ambiguity the terminal report and
/// the JSON export both take care to avoid.
fn schema_csv_rows(result: &SchemaDiffResult) -> Vec<String> {
    use crate::data::csv_escape;

    let row = |category: &str, column: &str, detail: &str, from: &str, to: &str, breaking: bool| {
        format!(
            "{},{},{},{},{},{}",
            csv_escape(category),
            csv_escape(column),
            csv_escape(detail),
            csv_escape(from),
            csv_escape(to),
            breaking
        )
    };

    let mut rows = vec!["category,column,detail,from,to,breaking".to_string()];

    for column in &result.added {
        rows.push(row("added_column", column, "", "", "", false));
    }
    for column in &result.removed {
        rows.push(row("removed_column", column, "", "", "", true));
    }
    for change in &result.type_changes {
        rows.push(row(
            "type_change",
            &change.column,
            &format!("{:?}", change.impact),
            &change.source_type,
            &change.target_type,
            change.impact != TypeChangeImpact::SafePromotion,
        ));
    }
    for change in &result.metadata.changes {
        rows.push(row(
            "metadata_change",
            change.column(),
            &change.to_string(),
            "",
            "",
            change.is_breaking(),
        ));
    }
    for (side, reason) in result.metadata.gaps() {
        rows.push(row("metadata_not_compared", side, &reason, "", "", false));
    }
    for rename in &result.rename_suggestions {
        rows.push(row(
            "rename_suggestion",
            &rename.source_column,
            &format!("confidence {:.2}", rename.score),
            &rename.source_column,
            &rename.target_column,
            false,
        ));
    }
    for reason in &result.compatibility.breaking_reasons {
        rows.push(row("breaking_reason", "", reason, "", "", true));
    }

    rows
}

/// Print a schema comparison.
fn render_schema_report(result: &SchemaDiffResult, policy_path: Option<&str>) {
    println!("Schema Comparison Results");
    println!("---------------------------");
    println!("Source file: {}", result.source_path);
    println!("Target file: {}", result.target_path);

    if result.added.is_empty() {
        println!("No columns added in target.");
    } else {
        println!("Columns added in target ({}): {:?}", result.added.len(), result.added);
    }

    if result.removed.is_empty() {
        println!("No columns removed from source.");
    } else {
        println!("Columns removed from source ({}): {:?}", result.removed.len(), result.removed);
    }

    if result.type_changes.is_empty() {
        println!("No type changes across shared columns.");
    } else {
        println!("Type changes in shared columns ({}):", result.type_changes.len());
        for change in &result.type_changes {
            println!(
                "  - {}: {} -> {} ({:?})",
                change.column, change.source_type, change.target_type, change.impact
            );
        }
    }

    render_metadata(&result.metadata);

    if result.rename_suggestions.is_empty() {
        println!("No strong rename candidates found.");
    } else {
        println!("Potential renames:");
        for rename in &result.rename_suggestions {
            println!(
                "  - {} -> {} (confidence {:.2})",
                rename.source_column, rename.target_column, rename.score
            );
        }
    }

    println!("Compatibility:");
    println!("  - Backward compatible: {}", result.compatibility.backward_compatible);
    println!("  - Forward compatible: {}", result.compatibility.forward_compatible);
    if result.compatibility.breaking_reasons.is_empty() {
        println!("  - Breaking reasons: none");
    } else {
        println!("  - Breaking reasons:");
        for reason in &result.compatibility.breaking_reasons {
            println!("    - {}", reason);
        }
    }

    if let Some(path) = policy_path {
        if result.policy_violations.is_empty() {
            println!("Policy check: passed ({})", path);
        } else {
            println!("Policy check: failed ({})", path);
            for violation in &result.policy_violations {
                println!("  - {}", violation);
            }
        }
    }
}

/// Print catalog findings, or say why there are none.
///
/// The silence case is the point. Without this, a comparison that never looked
/// at nullability is indistinguishable from one that looked and found nothing.
fn render_metadata(metadata: &MetadataReport) {
    if metadata.is_complete() {
        if metadata.changes.is_empty() {
            println!("No column metadata changes (types, nullability, defaults).");
        } else {
            println!("Column metadata changes ({}):", metadata.changes.len());
            for change in &metadata.changes {
                let marker = if change.is_breaking() { " [breaking]" } else { "" };
                println!("  - {}{}", change, marker);
            }
        }
        return;
    }

    println!("Column metadata not compared:");
    for (side, reason) in metadata.gaps() {
        println!("  - {}: {}", side, reason);
    }
}

fn schema_map(df: &DataFrame) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for field in df.schema().iter_fields() {
        map.insert(field.name().to_string(), format!("{:?}", field.data_type()));
    }
    Ok(map)
}

fn normalize_type_name(raw: &str) -> String {
    raw.to_ascii_lowercase().replace(' ', "")
}

fn numeric_rank(type_name: &str) -> Option<u8> {
    match normalize_type_name(type_name).as_str() {
        "int8" | "uint8" => Some(1),
        "int16" | "uint16" => Some(2),
        "int32" | "uint32" => Some(3),
        "int64" | "uint64" => Some(4),
        "float32" => Some(5),
        "float64" => Some(6),
        _ => None,
    }
}

fn classify_type_change(source_type: &str, target_type: &str) -> TypeChangeImpact {
    let source = normalize_type_name(source_type);
    let target = normalize_type_name(target_type);

    if source == target {
        return TypeChangeImpact::SafePromotion;
    }

    if source == "null" || target == "null" {
        return TypeChangeImpact::RiskyConversion;
    }

    if let (Some(source_rank), Some(target_rank)) = (numeric_rank(&source), numeric_rank(&target)) {
        return if target_rank >= source_rank {
            TypeChangeImpact::SafePromotion
        } else {
            TypeChangeImpact::RiskyConversion
        };
    }

    if (source.contains("date") || source.contains("datetime") || source.contains("time"))
        && target == "utf8"
    {
        return TypeChangeImpact::RiskyConversion;
    }

    if source == "utf8"
        && (target.contains("date") || target.contains("datetime") || target.contains("time"))
    {
        return TypeChangeImpact::RiskyConversion;
    }

    if (source == "boolean" && target.starts_with("int"))
        || (target == "boolean" && source.starts_with("int"))
    {
        return TypeChangeImpact::RiskyConversion;
    }

    TypeChangeImpact::Breaking
}

fn tokenize_name(name: &str) -> BTreeSet<String> {
    name.to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(|part| part.to_string())
        .collect()
}

fn name_similarity(a: &str, b: &str) -> f64 {
    let a_tokens = tokenize_name(a);
    let b_tokens = tokenize_name(b);

    if a_tokens.is_empty() || b_tokens.is_empty() {
        return 0.0;
    }

    let intersection = a_tokens.intersection(&b_tokens).count() as f64;
    let union = a_tokens.union(&b_tokens).count() as f64;
    let jaccard = if union > 0.0 { intersection / union } else { 0.0 };

    let a_norm = a.to_ascii_lowercase();
    let b_norm = b.to_ascii_lowercase();
    let prefix_bonus = if a_norm.starts_with(&b_norm) || b_norm.starts_with(&a_norm) {
        0.2
    } else {
        0.0
    };

    (jaccard + prefix_bonus).min(1.0)
}

fn detect_rename_suggestions(
    removed: &[String],
    added: &[String],
    source_schema: &HashMap<String, String>,
    target_schema: &HashMap<String, String>,
) -> Vec<RenameSuggestion> {
    let mut candidates: Vec<RenameSuggestion> = Vec::new();

    for source_col in removed {
        let Some(source_ty) = source_schema.get(source_col) else {
            continue;
        };

        for target_col in added {
            let Some(target_ty) = target_schema.get(target_col) else {
                continue;
            };

            if normalize_type_name(source_ty) != normalize_type_name(target_ty) {
                continue;
            }

            let score = name_similarity(source_col, target_col);
            if score >= 0.45 {
                candidates.push(RenameSuggestion {
                    source_column: source_col.clone(),
                    target_column: target_col.clone(),
                    score,
                });
            }
        }
    }

    candidates.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    // Greedy one-to-one matching to avoid noisy duplicate suggestions.
    let mut used_source = BTreeSet::new();
    let mut used_target = BTreeSet::new();
    let mut picked = Vec::new();
    for candidate in candidates {
        if used_source.contains(&candidate.source_column)
            || used_target.contains(&candidate.target_column)
        {
            continue;
        }
        used_source.insert(candidate.source_column.clone());
        used_target.insert(candidate.target_column.clone());
        picked.push(candidate);
    }

    picked
}

fn summarize_compatibility(
    added: &[String],
    removed: &[String],
    type_changes: &[TypeChange],
    metadata_changes: &[MetadataChange],
) -> CompatibilitySummary {
    let mut breaking_reasons = Vec::new();

    for col in removed {
        breaking_reasons.push(format!("Removed column: {col}"));
    }

    for change in type_changes {
        match change.impact {
            TypeChangeImpact::Breaking => {
                breaking_reasons.push(format!(
                    "Breaking type change: {} ({} -> {})",
                    change.column, change.source_type, change.target_type
                ));
            }
            TypeChangeImpact::RiskyConversion => {
                breaking_reasons.push(format!(
                    "Risky type change: {} ({} -> {})",
                    change.column, change.source_type, change.target_type
                ));
            }
            TypeChangeImpact::SafePromotion => {}
        }
    }

    // Catalog findings count towards the verdict. Without this the summary
    // reported "backward compatible: true, breaking reasons: none" directly
    // beneath changes the report had just marked [breaking] — a headline
    // contradicting its own detail, which is worse than not reporting at all.
    for change in metadata_changes {
        if change.is_breaking() {
            breaking_reasons.push(format!("Breaking metadata change: {change}"));
        }
    }

    let backward_compatible = breaking_reasons.is_empty();

    // Forward compatibility is stricter with added columns because old consumers may not expect them.
    let forward_compatible = removed.is_empty()
        && added.is_empty()
        && metadata_changes.is_empty()
        && type_changes
            .iter()
            .all(|change| change.impact == TypeChangeImpact::SafePromotion);

    CompatibilitySummary {
        backward_compatible,
        forward_compatible,
        breaking_reasons,
    }
}

fn load_policy(path: &str) -> Result<SchemaPolicy> {
    let raw = fs::read_to_string(path)?;
    let policy = serde_json::from_str::<SchemaPolicy>(&raw)
        .map_err(|err| anyhow!("Invalid schema policy JSON at {}: {}", path, err))?;
    Ok(policy)
}

fn type_change_allowed(policy: &SchemaPolicy, source_type: &str, target_type: &str) -> bool {
    let Some(allowed) = &policy.allowed_type_changes else {
        return false;
    };

    let from = normalize_type_name(source_type);
    let to = normalize_type_name(target_type);

    allowed.iter().any(|rule| {
        normalize_type_name(&rule.from) == from && normalize_type_name(&rule.to) == to
    })
}

fn evaluate_policy(
    policy: &SchemaPolicy,
    source_cols: &BTreeSet<String>,
    target_cols: &BTreeSet<String>,
    added: &[String],
    removed: &[String],
    type_changes: &[TypeChange],
    compatibility: &CompatibilitySummary,
) -> Vec<String> {
    let mut violations = Vec::new();

    if let Some(required_source) = &policy.required_columns_source {
        for col in required_source {
            if !source_cols.contains(col) {
                violations.push(format!("Missing required source column: {}", col));
            }
        }
    }

    if let Some(required_target) = &policy.required_columns_target {
        for col in required_target {
            if !target_cols.contains(col) {
                violations.push(format!("Missing required target column: {}", col));
            }
        }
    }

    if let Some(forbidden_removals) = &policy.forbidden_removals {
        for col in removed {
            if forbidden_removals.iter().any(|item| item == col) {
                violations.push(format!("Forbidden removal detected: {}", col));
            }
        }
    }

    if let Some(max_new_columns) = policy.max_new_columns {
        if added.len() > max_new_columns {
            violations.push(format!(
                "Added columns ({}) exceed max_new_columns ({})",
                added.len(),
                max_new_columns
            ));
        }
    }

    for change in type_changes {
        if change.impact == TypeChangeImpact::SafePromotion {
            continue;
        }

        if !type_change_allowed(policy, &change.source_type, &change.target_type) {
            violations.push(format!(
                "Disallowed type change: {} ({} -> {})",
                change.column, change.source_type, change.target_type
            ));
        }
    }

    if policy.fail_on_breaking.unwrap_or(true) && !compatibility.breaking_reasons.is_empty() {
        violations.push("Compatibility analysis found breaking/risky changes".to_string());
    }

    violations
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_numeric_promotion_as_safe() {
        assert_eq!(
            classify_type_change("Int32", "Int64"),
            TypeChangeImpact::SafePromotion
        );
    }

    #[test]
    fn classifies_numeric_narrowing_as_risky() {
        assert_eq!(
            classify_type_change("Int64", "Int32"),
            TypeChangeImpact::RiskyConversion
        );
    }

    #[test]
    fn similarity_detects_related_tokens() {
        let score = name_similarity("customer_id", "customer_identifier");
        assert!(score > 0.45);
    }
}
#[cfg(test)]
mod compatibility_tests {
    use super::*;
    use crate::catalog::MetadataChange;

    fn breaking_metadata() -> Vec<MetadataChange> {
        vec![MetadataChange::Nullability {
            column: "email".to_string(),
            now_nullable: true,
        }]
    }

    #[test]
    fn a_breaking_metadata_change_makes_the_verdict_incompatible() {
        // The report marks these [breaking] in its detail. If the summary did
        // not agree, it would print "backward compatible: true, breaking
        // reasons: none" directly beneath them.
        let summary = summarize_compatibility(&[], &[], &[], &breaking_metadata());

        assert!(!summary.backward_compatible);
        assert!(!summary.forward_compatible);
        assert_eq!(summary.breaking_reasons.len(), 1);
        assert!(summary.breaking_reasons[0].contains("NOT NULL dropped"));
    }

    #[test]
    fn the_verdict_never_contradicts_the_detail() {
        // The invariant, stated directly: if any reported change is breaking,
        // the summary must not claim backward compatibility.
        let changes = vec![
            MetadataChange::NativeType {
                column: "a".to_string(),
                from: "character varying(50)".to_string(),
                to: "text".to_string(),
                impact: crate::catalog::TypeImpact::Unknown,
            },
            MetadataChange::Default {
                column: "b".to_string(),
                from: None,
                to: Some("0".to_string()),
            },
        ];
        let summary = summarize_compatibility(&[], &[], &[], &changes);

        let any_breaking = changes.iter().any(|c| c.is_breaking());
        assert_eq!(summary.backward_compatible, !any_breaking);
    }

    #[test]
    fn non_breaking_metadata_still_blocks_forward_compatibility() {
        // A new default does not break a reader, but an old consumer written
        // against the previous schema is no longer looking at the same table.
        let changes = vec![MetadataChange::Default {
            column: "n".to_string(),
            from: None,
            to: Some("0".to_string()),
        }];
        let summary = summarize_compatibility(&[], &[], &[], &changes);

        assert!(summary.backward_compatible, "readers are unaffected");
        assert!(!summary.forward_compatible);
    }

    #[test]
    fn no_metadata_changes_leaves_the_verdict_untouched() {
        let summary = summarize_compatibility(&[], &[], &[], &[]);
        assert!(summary.backward_compatible);
        assert!(summary.forward_compatible);
        assert!(summary.breaking_reasons.is_empty());
    }
}

#[cfg(test)]
mod export_tests {
    use super::*;
    use crate::catalog::{CatalogAvailability, ColumnDef, MetadataChange, TableCatalog};

    fn column(name: &str, data_type: &str, nullable: bool) -> ColumnDef {
        ColumnDef {
            name: name.to_string(),
            data_type: data_type.to_string(),
            nullable,
            default: None,
            ordinal: 1,
        }
    }

    fn result_with(metadata: MetadataReport) -> SchemaDiffResult {
        SchemaDiffResult {
            source_path: "a".into(),
            target_path: "b".into(),
            source_schema: Default::default(),
            target_schema: Default::default(),
            added: vec![],
            removed: vec![],
            type_changes: vec![],
            rename_suggestions: vec![],
            compatibility: CompatibilitySummary {
                backward_compatible: true,
                forward_compatible: true,
                breaking_reasons: vec![],
            },
            metadata,
            policy_violations: vec![],
            policy_passed: None,
        }
    }

    #[test]
    fn csv_states_when_metadata_was_never_examined() {
        // Without these rows, a CSV with no metadata_change lines is
        // indistinguishable from one where the catalog was never read. The
        // terminal report and JSON both avoid that ambiguity; CSV must too.
        let rows = schema_csv_rows(&result_with(MetadataReport {
            source: CatalogAvailability::NotADatabase,
            target: CatalogAvailability::UnsupportedEngine("MySQL"),
            changes: vec![],
        }));

        let gaps: Vec<&String> = rows
            .iter()
            .filter(|r| r.starts_with("metadata_not_compared"))
            .collect();
        assert_eq!(gaps.len(), 2, "one per side: {rows:?}");
        assert!(gaps.iter().any(|r| r.contains("no catalog")));
        assert!(gaps.iter().any(|r| r.contains("MySQL")));
    }

    #[test]
    fn csv_omits_gap_rows_when_both_sides_were_read() {
        let rows = schema_csv_rows(&result_with(MetadataReport {
            source: CatalogAvailability::Available(TableCatalog {
                columns: vec![column("id", "bigint", false)],
            }),
            target: CatalogAvailability::Available(TableCatalog {
                columns: vec![column("id", "bigint", false)],
            }),
            changes: vec![],
        }));

        assert!(!rows.iter().any(|r| r.starts_with("metadata_not_compared")));
    }

    #[test]
    fn csv_marks_breaking_changes() {
        let rows = schema_csv_rows(&result_with(MetadataReport {
            source: CatalogAvailability::Available(TableCatalog::default()),
            target: CatalogAvailability::Available(TableCatalog::default()),
            changes: vec![
                MetadataChange::Nullability {
                    column: "email".into(),
                    now_nullable: true,
                },
                MetadataChange::Default {
                    column: "n".into(),
                    from: None,
                    to: Some("0".into()),
                },
            ],
        }));

        let nullability = rows.iter().find(|r| r.contains("NOT NULL dropped")).unwrap();
        assert!(nullability.ends_with(",true"), "{nullability}");
        let default = rows.iter().find(|r| r.contains("default none -> 0")).unwrap();
        assert!(default.ends_with(",false"), "{default}");
    }

    #[test]
    fn csv_has_a_header_and_a_stable_column_count() {
        let rows = schema_csv_rows(&result_with(MetadataReport {
            source: CatalogAvailability::NotADatabase,
            target: CatalogAvailability::NotADatabase,
            changes: vec![],
        }));

        assert_eq!(rows[0], "category,column,detail,from,to,breaking");
        let fields = rows[0].split(',').count();
        for row in &rows[1..] {
            assert_eq!(row.split(',').count(), fields, "ragged row: {row}");
        }
    }
}
