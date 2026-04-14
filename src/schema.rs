use anyhow::{anyhow, Result};
use polars::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::fs;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
enum TypeChangeImpact {
    SafePromotion,
    RiskyConversion,
    Breaking,
}

#[derive(Clone, Debug)]
pub enum SchemaDiffError {
    MissingColumnType(String),
    PolicyViolation(String),
    InvalidPolicyFile(String),
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
    column: String,
    source_type: String,
    target_type: String,
    impact: TypeChangeImpact,
}

#[derive(Debug, Clone, Serialize)]
pub struct RenameSuggestion {
    source_column: String,
    target_column: String,
    score: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompatibilitySummary {
    backward_compatible: bool,
    forward_compatible: bool,
    breaking_reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SchemaDiffResult {
    pub source_path: String,
    pub target_path: String,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub type_changes: Vec<TypeChange>,
    pub rename_suggestions: Vec<RenameSuggestion>,
    pub compatibility: CompatibilitySummary,
    pub policy_violations: Vec<String>,
    pub policy_passed: Option<bool>,
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
    run_schema_diff_inner(&df1, &df2, source_label, target_label, None)
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

    run_schema_diff_inner(&df1, &df2, path1, path2, policy_path)
}

fn run_schema_diff_inner(df1: &DataFrame, df2: &DataFrame, source_label: &str, target_label: &str, policy_path: Option<&str>) -> Result<SchemaDiffResult, SchemaDiffError> {
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
    let compatibility = summarize_compatibility(&added, &removed, &type_changes);

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
        added,
        removed,
        type_changes,
        rename_suggestions,
        compatibility,
        policy_violations,
        policy_passed,
    })
}

pub fn schema_diff(path1: &str, path2: &str, policy_path: Option<&str>) -> Result<(), SchemaDiffError> {
    let df1 = CsvReader::from_path(path1)?
        .infer_schema(Some(100))
        .has_header(true)
        .finish()?;

    let df2 = CsvReader::from_path(path2)?
        .infer_schema(Some(100))
        .has_header(true)
        .finish()?;

    let source_schema = schema_map(&df1)?;
    let target_schema = schema_map(&df2)?;

    let source_cols: BTreeSet<String> = source_schema.keys().cloned().collect();
    let target_cols: BTreeSet<String> = target_schema.keys().cloned().collect();

    let added: Vec<String> = target_cols.difference(&source_cols).cloned().collect();
    let removed: Vec<String> = source_cols.difference(&target_cols).cloned().collect();

    let mut type_changes = Vec::new();
    for col in source_cols.intersection(&target_cols) {
        let source_ty = source_schema
            .get(col)
            .ok_or_else(|| anyhow!("Missing source type for column: {col}"))?;
        let target_ty = target_schema
            .get(col)
            .ok_or_else(|| anyhow!("Missing target type for column: {col}"))?;

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
    let compatibility = summarize_compatibility(&added, &removed, &type_changes);

    println!("Schema Comparison Results");
    println!("---------------------------");
    println!("Source file: {}", path1);
    println!("Target file: {}", path2);

    if added.is_empty() {
        println!("No columns added in target.");
    } else {
        println!("Columns added in target ({}): {:?}", added.len(), added);
    }

    if removed.is_empty() {
        println!("No columns removed from source.");
    } else {
        println!("Columns removed from source ({}): {:?}", removed.len(), removed);
    }

    if type_changes.is_empty() {
        println!("No type changes across shared columns.");
    } else {
        println!("Type changes in shared columns ({}):", type_changes.len());
        for change in &type_changes {
            println!(
                "  - {}: {} -> {} ({:?})",
                change.column, change.source_type, change.target_type, change.impact
            );
        }
    }

    if rename_suggestions.is_empty() {
        println!("No strong rename candidates found.");
    } else {
        println!("Potential renames:");
        for rename in &rename_suggestions {
            println!(
                "  - {} -> {} (confidence {:.2})",
                rename.source_column, rename.target_column, rename.score
            );
        }
    }

    println!("Compatibility:");
    println!("  - Backward compatible: {}", compatibility.backward_compatible);
    println!("  - Forward compatible: {}", compatibility.forward_compatible);
    if compatibility.breaking_reasons.is_empty() {
        println!("  - Breaking reasons: none");
    } else {
        println!("  - Breaking reasons:");
        for reason in &compatibility.breaking_reasons {
            println!("    - {}", reason);
        }
    }

    if let Some(path) = policy_path {
        let policy = load_policy(path)?;
        let violations = evaluate_policy(
            &policy,
            &source_cols,
            &target_cols,
            &added,
            &removed,
            &type_changes,
            &compatibility,
        );

        if violations.is_empty() {
            println!("Policy check: passed ({})", path);
        } else {
            println!("Policy check: failed ({})", path);
            for violation in &violations {
                println!("  - {}", violation);
            }

            if policy.fail_on_breaking.unwrap_or(true) {
                return Err(SchemaDiffError::PolicyViolation("Schema policy violations detected".to_string()));
            }
        }
    }

    Ok(())
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

    let backward_compatible = breaking_reasons.is_empty();

    // Forward compatibility is stricter with added columns because old consumers may not expect them.
    let forward_compatible = removed.is_empty()
        && added.is_empty()
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